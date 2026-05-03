// CSS Grid layout — track sizing, named lines, grid-area placement,
// occupancy-based auto-flow.

use crate::{
    css::{GridLine, TrackSize, Unit, Value},
    style::StyledNode,
};

use super::{
    LayoutBox, is_out_of_flow, length_value, outer_rect,
    shift_layout_subtree,
};
use super::inline::{layout_inline_block_node, layout_inline_or_inline_block};

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
    // Four-pass placement.
    //   Pass 0: assign each in-flow item a GridArea via grid-column/grid-row
    //           (when explicit) plus row-major auto-flow filling around the
    //           explicit cells. Items spanning multiple cells claim a
    //           rectangular block. Then lay each item out at the container
    //           origin to measure natural outer widths.
    //   Pass 1: resolve column tracks. Auto tracks pick the per-column max
    //           natural width (spanned items contribute to col_start only —
    //           a known toy simplification of spec's "distribute across
    //           spanned tracks" rule).
    //   Pass 2: shift each item to its column-start track and grow its
    //           content to fill the spanned columns when no explicit width
    //           was declared.
    //   Pass 3: resolve row heights via grid-template-rows + natural per-row
    //           max. Shift each item down by its row-start offset and grow
    //           content height to fill spanned rows when no explicit height.
    //
    // Out-of-flow children skip the grid entirely — they sit at the container
    // origin during pass 0 and the absolute reposition pass at the tree root
    // moves them to their containing block.
    let track_decls = match container.value("grid-template-columns") {
        Some(Value::TrackList(tracks)) if !tracks.is_empty() => Some(tracks.as_slice()),
        _ => None,
    };
    let n_cols = track_decls.map(|t| t.len()).unwrap_or(1).max(1);

    let mut boxes: Vec<LayoutBox> = Vec::with_capacity(children.len());
    // For each in-flow item: its area + boxes index + source styled node.
    let mut cell_assignments: Vec<(GridArea, usize, &'a StyledNode)> = Vec::new();
    let mut occupancy = Occupancy::new(n_cols);
    let mut cursor = (0usize, 0usize);

    for child in children {
        if is_out_of_flow(child) {
            let abs_box = layout_inline_or_inline_block(child, content_x, content_y, content_width);
            boxes.push(abs_box);
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
        let box_idx = boxes.len();
        cell_assignments.push((area, box_idx, child));
        // Pre-pass: lay out at the container origin so we can read the
        // item's natural outer width before knowing its track width.
        boxes.push(layout_inline_block_node(
            child,
            content_x,
            content_y,
            content_width,
        ));
    }

    // Per-column natural max outer width — feeds auto track sizing. Spanned
    // items contribute to col_start only (toy simplification).
    let mut natural_max_per_col = vec![0.0_f32; n_cols];
    for &(area, box_idx, _) in &cell_assignments {
        let w = outer_rect(&boxes[box_idx]).width;
        if w > natural_max_per_col[area.col_start] {
            natural_max_per_col[area.col_start] = w;
        }
    }

    let columns = resolve_grid_columns(track_decls, content_width, &natural_max_per_col);
    let mut col_offsets: Vec<f32> = Vec::with_capacity(columns.len());
    let mut acc = 0.0;
    for w in &columns {
        col_offsets.push(acc);
        acc += w;
    }

    // Pass 2: shift each item to its track-start x and grow content to fill
    // the spanned columns (sum of widths from col_start..col_end).
    for &(area, box_idx, child) in &cell_assignments {
        let target_outer_x = content_x + col_offsets[area.col_start];
        let current_outer_x = outer_rect(&boxes[box_idx]).x;
        let dx = target_outer_x - current_outer_x;
        if dx != 0.0 {
            shift_layout_subtree(&mut boxes[box_idx], dx, 0.0);
        }
        if !matches!(child.value("width"), Some(Value::Length(_, _))) {
            let span_width: f32 = columns[area.col_start..area.col_end.min(columns.len())]
                .iter()
                .sum();
            let edges =
                outer_rect(&boxes[box_idx]).width - boxes[box_idx].dimensions.content.width;
            let target = (span_width - edges).max(0.0);
            if boxes[box_idx].dimensions.content.width < target {
                boxes[box_idx].dimensions.content.width = target;
            }
        }
    }

    // Pass 3: natural row heights = max(item outer height) per row, with
    // spanned items contributing to row_start only.
    let n_rows = cell_assignments
        .iter()
        .map(|&(area, _, _)| area.row_end)
        .max()
        .unwrap_or(0);
    let mut natural_row_heights = vec![0.0_f32; n_rows];
    for &(area, box_idx, _) in &cell_assignments {
        let h = outer_rect(&boxes[box_idx]).height;
        if h > natural_row_heights[area.row_start] {
            natural_row_heights[area.row_start] = h;
        }
    }

    let row_track_decls = match container.value("grid-template-rows") {
        Some(Value::TrackList(tracks)) if !tracks.is_empty() => Some(tracks.as_slice()),
        _ => None,
    };
    let container_explicit_height = length_value(container, "height", content_width);
    let row_heights = resolve_grid_rows(
        row_track_decls,
        container_explicit_height,
        &natural_row_heights,
    );

    let mut row_offsets: Vec<f32> = Vec::with_capacity(n_rows);
    let mut acc = 0.0;
    for h in &row_heights {
        row_offsets.push(acc);
        acc += h;
    }
    for &(area, box_idx, child) in &cell_assignments {
        let dy = row_offsets[area.row_start];
        if dy != 0.0 {
            shift_layout_subtree(&mut boxes[box_idx], 0.0, dy);
        }
        // Fill content to span the row range when no explicit height. This
        // covers single-row and multi-row spans uniformly: an item without
        // its own height takes the row's resolved height (or the sum across
        // a multi-row span). Items with explicit height keep their declared
        // size — this matches the column-track post-hoc fill on the main
        // axis.
        if !matches!(child.value("height"), Some(Value::Length(_, _))) {
            let span_height: f32 = row_heights[area.row_start..area.row_end.min(row_heights.len())]
                .iter()
                .sum();
            let edges = outer_rect(&boxes[box_idx]).height
                - boxes[box_idx].dimensions.content.height;
            let target = (span_height - edges).max(0.0);
            if boxes[box_idx].dimensions.content.height < target {
                boxes[box_idx].dimensions.content.height = target;
            }
        }
    }

    let auto_content_height: f32 = row_heights.iter().sum();
    (boxes, auto_content_height)
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
    match (placement.start, placement.end) {
        (GridLine::Index(s), GridLine::Index(e)) if e > s => AxisHint {
            explicit_start: Some((s - 1) as usize),
            span: (e - s) as usize,
        },
        (GridLine::Index(s), GridLine::Span(n)) => AxisHint {
            explicit_start: Some((s - 1) as usize),
            span: n.max(1) as usize,
        },
        (GridLine::Index(s), GridLine::Auto) => AxisHint {
            explicit_start: Some((s - 1) as usize),
            span: 1,
        },
        // `span <n>` as the start side: no explicit anchor, but the item
        // claims n cells when auto-placed.
        (GridLine::Span(n), _) => AxisHint {
            explicit_start: None,
            span: n.max(1) as usize,
        },
        // Anything else (e.g. `auto / 3` — end-only) falls back to a
        // single-cell auto placement. Toy doesn't try to honor end-only
        // line constraints because they need a back-search through the
        // already-placed items.
        _ => AxisHint {
            explicit_start: None,
            span: 1,
        },
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

fn resolve_grid_rows(
    tracks: Option<&[TrackSize]>,
    container_height: Option<f32>,
    natural_row_heights: &[f32],
) -> Vec<f32> {
    // Rows differ from columns in two ways:
    //   - Container main-axis size (height) is often `auto`. fr rows can only
    //     distribute leftover when the container has an explicit height; under
    //     auto height they collapse to zero (matching the flex-column rule).
    //   - The template can be shorter than the implicit row count (more
    //     items than declared rows). Trailing rows beyond the template fall
    //     back to natural max — the same auto-fallback that took row sizing
    //     before this commit.
    let template = tracks.unwrap_or(&[]);
    let n_rows = natural_row_heights.len();
    if n_rows == 0 {
        return Vec::new();
    }

    let mut sizes = vec![0.0_f32; n_rows];
    let mut total_fr = 0.0_f32;
    let mut fixed_total = 0.0_f32;
    for (i, &natural_h) in natural_row_heights.iter().enumerate() {
        if let Some(track) = template.get(i) {
            match track {
                TrackSize::Length(value, Unit::Px) => {
                    sizes[i] = *value;
                    fixed_total += *value;
                }
                TrackSize::Length(value, Unit::Percent) => {
                    let resolved = *value / 100.0 * container_height.unwrap_or(0.0);
                    sizes[i] = resolved;
                    fixed_total += resolved;
                }
                TrackSize::Length(value, _) => {
                    sizes[i] = *value;
                    fixed_total += *value;
                }
                TrackSize::Auto => {
                    sizes[i] = natural_h;
                    fixed_total += natural_h;
                }
                TrackSize::Fraction(weight) => {
                    total_fr += *weight;
                    // Filled in below if container_height is known.
                }
            }
        } else {
            sizes[i] = natural_h;
            fixed_total += natural_h;
        }
    }

    if total_fr > 0.0
        && let Some(container_h) = container_height
    {
        let free = (container_h - fixed_total).max(0.0);
        for (i, track) in template.iter().enumerate().take(n_rows) {
            if let TrackSize::Fraction(weight) = track {
                sizes[i] = free * *weight / total_fr;
            }
        }
    }

    sizes
}

fn resolve_grid_columns(
    tracks: Option<&[TrackSize]>,
    content_width: f32,
    natural_max_per_col: &[f32],
) -> Vec<f32> {
    // Resolves `grid-template-columns` to a Vec of pixel widths. Length and
    // Auto tracks contribute fixed budget; Fraction tracks split the leftover
    // proportionally to their weight, like flex-grow. With no declaration,
    // behave like a single full-width track so a bare `display: grid` still
    // produces sensible single-column output.
    let tracks = match tracks {
        Some(t) if !t.is_empty() => t,
        _ => return vec![content_width],
    };

    let mut fixed_total = 0.0_f32;
    let mut total_fr = 0.0_f32;
    for (i, track) in tracks.iter().enumerate() {
        match track {
            TrackSize::Length(value, Unit::Px) => fixed_total += *value,
            TrackSize::Length(value, Unit::Percent) => {
                fixed_total += *value / 100.0 * content_width;
            }
            // em/rem are resolved to Px during style; this fallback only
            // matters if a future code path bypasses style-time resolution.
            TrackSize::Length(value, _) => fixed_total += *value,
            TrackSize::Auto => fixed_total += natural_max_per_col.get(i).copied().unwrap_or(0.0),
            TrackSize::Fraction(weight) => total_fr += *weight,
        }
    }
    let free = (content_width - fixed_total).max(0.0);

    tracks
        .iter()
        .enumerate()
        .map(|(i, track)| match track {
            TrackSize::Length(value, Unit::Px) => *value,
            TrackSize::Length(value, Unit::Percent) => *value / 100.0 * content_width,
            TrackSize::Length(value, _) => *value,
            TrackSize::Auto => natural_max_per_col.get(i).copied().unwrap_or(0.0),
            TrackSize::Fraction(weight) if total_fr > 0.0 => free * *weight / total_fr,
            TrackSize::Fraction(_) => 0.0,
        })
        .collect()
}
