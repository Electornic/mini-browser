// CSS table layout — `display: table` / `table-row` / `table-cell` plus the
// row-group transparency for thead/tbody/tfoot. The algorithm is the
// auto-table flavour: column widths come from per-cell max-content
// measurements (with explicit `width` declarations contributing as a
// hint), then proportionally scaled to fit the table's content width.
// Each row's height is the tallest cell in that row, and every cell in
// the row is grown to that height so the row paints as one strip.
//
// What's NOT supported (deliberate scope cuts for the toy renderer):
//   - colspan / rowspan
//   - border-collapse (cells always paint with their own border, gaps
//     between cells are border-spacing)
//   - <caption> placement above/below the table
//   - <col> / <colgroup> column styling
//   - separate min-content vs max-content tracks (we only measure max)
//   - table-cell vertical-align (cells stretched to row height paint
//     their content from the top)

use crate::{css::Value, dom::NodeType, style::StyledNode};

use super::{
    Dimensions, LayoutBox, Rect, container_box_type, edge_sizes, intrinsic_height,
    is_display_none, is_layout_whitespace_text, length_value, outer_rect,
};
use super::block::layout_node;
use super::inline::layout_inline_block_node;

// Probe width for max-content measurement. A bounded sentinel (rather than
// f32::INFINITY) keeps any downstream multiplication / addition finite even
// in pathological author CSS (e.g. percent margins on a probed cell).
const MAX_PROBE_WIDTH: f32 = 100_000.0;

pub(super) fn is_table_container(node: &StyledNode) -> bool {
    matches!(node.value("display"), Some(Value::Keyword(kw)) if kw == "table")
}

fn is_table_row(node: &StyledNode) -> bool {
    matches!(node.value("display"), Some(Value::Keyword(kw)) if kw == "table-row")
}

fn is_table_cell(node: &StyledNode) -> bool {
    matches!(node.value("display"), Some(Value::Keyword(kw)) if kw == "table-cell")
}

fn is_table_row_group(node: &StyledNode) -> bool {
    matches!(
        node.value("display"),
        Some(Value::Keyword(kw))
            if kw == "table-row-group"
                || kw == "table-header-group"
                || kw == "table-footer-group"
    )
}

pub(super) fn layout_table_children(
    container: &StyledNode,
    children: &[StyledNode],
    content_x: f32,
    content_y: f32,
    content_width: f32,
) -> (Vec<LayoutBox>, f32) {
    // Pass A: harvest rows, descending transparently through row-group
    // children (thead / tbody / tfoot) so any <tr> that lives under a group
    // becomes a sibling of <tr>s that sit directly under the table. Hidden
    // rows (display:none) are dropped here so they never contribute to
    // column or row sizing.
    let rows = collect_rows(children);
    if rows.is_empty() {
        return (Vec::new(), 0.0);
    }

    // Pass B: each row's cell list, in document order. Non-cell children
    // (raw text whitespace, stray elements) are dropped so the row is
    // exactly its <td>/<th> sequence.
    let row_cells: Vec<Vec<&StyledNode>> = rows
        .iter()
        .map(|row| collect_cells(&row.children))
        .collect();
    // Column count is the widest row measured by *colspan sum*, not by
    // physical cell count. HN's subtext row is `<td colspan="2"></td>
    // <td>…</td>` (two cells but three columns); without honouring the
    // colspan, the subtext column would land in column 1 instead of
    // column 2 and never align with the title row above it.
    let column_count = row_cells
        .iter()
        .map(|cells| cells.iter().map(|c| cell_colspan(c)).sum::<usize>())
        .max()
        .unwrap_or(0);
    if column_count == 0 {
        return (Vec::new(), 0.0);
    }

    // Pass C: per-column max-content measurement. For every cell, ask "how
    // wide would you want to be if no column constraint applied?" and keep
    // the largest answer per column. Explicit `width` on a cell flows
    // through layout_inline_block_node automatically (its first sizing
    // branch reads the declared width). Multi-column (colspan > 1) cells
    // are skipped here — they want a width spanning several columns, but
    // the proper distribution algorithm is intricate; the toy lets the
    // single-column cells drive sizing and lets the spanned cell take
    // whatever total the columns add up to. That undersizes a few cases
    // (a colspan cell wider than the columns combined) but matches the
    // common HN layout where multi-column cells are spacers/empty.
    let mut col_widths = vec![0.0_f32; column_count];
    for cells in &row_cells {
        let mut col_idx = 0usize;
        for cell in cells {
            let span = cell_colspan(cell).min(column_count.saturating_sub(col_idx));
            if span == 1 {
                let measured = measure_cell_max_width(cell);
                if measured > col_widths[col_idx] {
                    col_widths[col_idx] = measured;
                }
            }
            col_idx += span.max(1);
        }
    }

    // Pass D: scale column widths so the row strip fits the table's content
    // box. CSS table-layout spec is intricate (min-content vs max-content,
    // distributing leftover by various heuristics); the toy uses a flat
    // proportional scale. Only scale UP when the table has an explicit
    // width — without that signal an "auto" table would stretch a 2-column
    // header across the entire viewport, which is exactly the HN
    // anti-pattern table layout is supposed to avoid.
    let border_spacing = length_value(container, "border-spacing", content_width).unwrap_or(0.0);
    // border-spacing applies between every adjacent pair of cells PLUS at
    // the outer edges (between table border and the outermost cells), so
    // the total horizontal gap consumed is (column_count + 1) × spacing.
    let total_gap = (column_count as f32 + 1.0) * border_spacing;
    let available_for_cells = (content_width - total_gap).max(0.0);
    let total_natural: f32 = col_widths.iter().sum();
    let table_has_explicit_width = matches!(container.value("width"), Some(Value::Length(..)));
    if total_natural > 0.0 {
        let must_shrink = total_natural > available_for_cells;
        let want_grow = table_has_explicit_width && total_natural < available_for_cells;
        if must_shrink || want_grow {
            let scale = available_for_cells / total_natural;
            for w in col_widths.iter_mut() {
                *w *= scale;
            }
        }
    }

    // Pre-compute each column's left edge (the outer x of a cell that
    // starts at column j). Sequential summation keeps a colspan cell's
    // x lookup as a single index and lets its width derive from the
    // difference between the start of column[j] and column[j+span].
    let mut column_x_starts = vec![0.0_f32; column_count + 1];
    {
        let mut x = content_x + border_spacing;
        for j in 0..column_count {
            column_x_starts[j] = x;
            x += col_widths[j] + border_spacing;
        }
        column_x_starts[column_count] = x;
    }

    // Pass E: lay out each row with the resolved column widths. Single-
    // column cells take their column's width; colspan cells span several
    // adjacent columns plus the (span - 1) border-spacings between them.
    // Cells in the same column share an x-offset and width; cells in the
    // same row share a y-offset and height (row height = tallest cell
    // after layout). Every cell is then grown to the row height so the
    // row paints as one continuous strip — without this, a tall cell
    // next to a short cell makes the row look ragged and any per-cell
    // background colour bleeds beyond the visible row outline.
    let mut all_children: Vec<LayoutBox> = Vec::new();
    let mut cursor_y = content_y + border_spacing;
    for cells in &row_cells {
        let row_start = all_children.len();
        let mut col_idx = 0usize;
        let mut row_outer_height = 0.0_f32;
        for cell in cells {
            if col_idx >= column_count {
                // Excess cells past the table's widest row simply drop —
                // a malformed input pattern that real browsers also
                // tolerate by ignoring the overflow cells.
                break;
            }
            let span = cell_colspan(cell).min(column_count - col_idx).max(1);
            let cell_outer_width =
                (column_x_starts[col_idx + span] - column_x_starts[col_idx] - border_spacing)
                    .max(0.0);
            let cell_box = layout_table_cell(
                cell,
                column_x_starts[col_idx],
                cursor_y,
                cell_outer_width,
            );
            let outer_h = outer_rect(&cell_box).height;
            if outer_h > row_outer_height {
                row_outer_height = outer_h;
            }
            all_children.push(cell_box);
            col_idx += span;
        }
        for cell_box in &mut all_children[row_start..] {
            grow_cell_to_row_height(cell_box, row_outer_height);
        }
        cursor_y += row_outer_height + border_spacing;
    }

    let auto_height = (cursor_y - content_y).max(0.0);
    (all_children, auto_height)
}

fn collect_rows(children: &[StyledNode]) -> Vec<&StyledNode> {
    let mut rows = Vec::new();
    for child in children {
        if is_display_none(child) {
            continue;
        }
        if is_table_row(child) {
            rows.push(child);
        } else if is_table_row_group(child) {
            for grandchild in &child.children {
                if !is_display_none(grandchild) && is_table_row(grandchild) {
                    rows.push(grandchild);
                }
            }
        }
    }
    rows
}

fn collect_cells(children: &[StyledNode]) -> Vec<&StyledNode> {
    children
        .iter()
        .filter(|child| !is_display_none(child) && is_table_cell(child))
        .collect()
}

// Read the `colspan` attribute on a `<td>`/`<th>` cell. The HTML default
// (per spec and per every browser) is 1; anything missing, malformed,
// zero, or negative collapses to that default. Values larger than the
// table's column count are clamped at use sites — keeping the parse
// permissive lets garbage-in-the-wild ride through without panicking.
fn cell_colspan(cell: &StyledNode) -> usize {
    let NodeType::Element(element) = &cell.node_type else {
        return 1;
    };
    element
        .attributes
        .get("colspan")
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|n| *n >= 1)
        .unwrap_or(1)
}

fn measure_cell_max_width(cell: &StyledNode) -> f32 {
    // Run the cell through the inline-block path with effectively unbounded
    // available width. Whatever outer width comes back is what the cell
    // would *want* if no column constraint applied — that becomes the
    // cell's contribution to the column's max-content width. The probe
    // box is thrown away; the real layout in Pass E builds a fresh box at
    // the resolved cell width.
    let probe = layout_inline_block_node(cell, 0.0, 0.0, MAX_PROBE_WIDTH);
    outer_rect(&probe).width
}

fn layout_table_cell(cell: &StyledNode, x: f32, y: f32, cell_outer_width: f32) -> LayoutBox {
    // The column has already decided this cell's outer width — author CSS
    // on the cell's `width` was a hint that fed column sizing in Pass C,
    // not a binding constraint here. Margins / borders / padding stay as
    // declared, and whatever space remains becomes the content width.
    // Standard block dispatch handles whatever the cell holds (inline
    // text, stacked block children, even a nested table).
    let margin = edge_sizes(cell, "margin", cell_outer_width);
    let padding = edge_sizes(cell, "padding", cell_outer_width);
    let border = edge_sizes(cell, "border", cell_outer_width);
    let horizontal_non_content = margin.left
        + margin.right
        + border.left
        + border.right
        + padding.left
        + padding.right;
    let content_width = (cell_outer_width - horizontal_non_content).max(0.0);
    let content_x = x + margin.left + border.left + padding.left;
    let content_y = y + margin.top + border.top + padding.top;

    // Honour display:none on the cell's children before recursing — block
    // flow does the same filter, so cells stay in step with the rest of
    // the layout pipeline.
    let mut child_cursor_y = content_y;
    let mut children: Vec<LayoutBox> = Vec::new();
    for child in &cell.children {
        if is_display_none(child) {
            continue;
        }
        // Inter-element whitespace text (preserved by the parser to keep
        // inline runs spaced) would otherwise lay out as a font-size-tall
        // full-width row inside the cell — a phantom block strip stacked
        // above / below the real cell content. Block flow drops the same
        // pattern for the same reason.
        if is_layout_whitespace_text(child) {
            continue;
        }
        children.push(layout_node(child, content_x, &mut child_cursor_y, content_width));
    }
    let auto_content_height = (child_cursor_y - content_y).max(0.0);
    let content_height = length_value(cell, "height", cell_outer_width)
        .unwrap_or_else(|| auto_content_height.max(intrinsic_height(cell)));

    LayoutBox {
        box_type: container_box_type(cell),
        dimensions: Dimensions {
            content: Rect {
                x: content_x,
                y: content_y,
                width: content_width,
                height: content_height,
            },
            padding,
            border,
            margin,
        },
        children,
    }
}

fn grow_cell_to_row_height(cell_box: &mut LayoutBox, row_outer_height: f32) {
    // Pad the cell's content height so its outer height matches the
    // tallest cell in the row. Only grow — never shrink — so a cell that
    // already exceeds row_outer_height (shouldn't happen given the way
    // row_outer_height is computed, but cheap to defend against) keeps
    // its true height.
    let outer_h = outer_rect(cell_box).height;
    if outer_h >= row_outer_height {
        return;
    }
    cell_box.dimensions.content.height += row_outer_height - outer_h;
}

#[cfg(test)]
mod tests {
    use crate::{css, html, layout::layout_tree, style};

    fn styled_root(html_source: &str, css_source: &str) -> style::StyledNode {
        let document = html::parse(html_source).unwrap();
        let root = document.roots()[0];
        let stylesheet = css::parse(css_source).unwrap();
        style::style_tree(&document, root, &[stylesheet])
    }

    #[test]
    fn places_cells_side_by_side_in_a_single_row() {
        // Two cells with image content of distinct widths drive distinct
        // column widths. With border-spacing 0 and zero padding, the
        // second cell's outer left edge sits exactly where the first
        // cell's outer right edge ended.
        let styled = styled_root(
            r#"<table><tr><td><img width="40" height="20"></td><td><img width="80" height="20"></td></tr></table>"#,
            r#"table { border-spacing: 0; } td { padding: 0; }"#,
        );
        let layout = layout_tree(&styled, 400.0);
        assert_eq!(layout.children.len(), 2);
        let cell0 = &layout.children[0];
        let cell1 = &layout.children[1];
        assert_eq!(cell0.dimensions.content.width, 40.0);
        assert_eq!(cell1.dimensions.content.width, 80.0);
        assert_eq!(cell0.dimensions.content.y, cell1.dimensions.content.y);
        assert_eq!(
            cell1.dimensions.content.x,
            cell0.dimensions.content.x + 40.0
        );
    }

    #[test]
    fn aligns_cells_in_the_same_column_across_rows() {
        // Column 0 width is max(60, 20) = 60; column 1 width is
        // max(40, 100) = 100. Same-column cells must share x and width
        // — that is the entire point of having a table layout instead of
        // independent block-flow rows.
        let styled = styled_root(
            r#"<table>
                <tr><td><img width="60" height="20"></td><td><img width="40" height="20"></td></tr>
                <tr><td><img width="20" height="20"></td><td><img width="100" height="20"></td></tr>
            </table>"#,
            r#"table { border-spacing: 0; } td { padding: 0; }"#,
        );
        let layout = layout_tree(&styled, 400.0);
        assert_eq!(layout.children.len(), 4);
        let row0_col0 = &layout.children[0];
        let row0_col1 = &layout.children[1];
        let row1_col0 = &layout.children[2];
        let row1_col1 = &layout.children[3];
        assert_eq!(row0_col0.dimensions.content.width, 60.0);
        assert_eq!(row1_col0.dimensions.content.width, 60.0);
        assert_eq!(row0_col1.dimensions.content.width, 100.0);
        assert_eq!(row1_col1.dimensions.content.width, 100.0);
        assert_eq!(
            row0_col0.dimensions.content.x,
            row1_col0.dimensions.content.x
        );
        assert_eq!(
            row0_col1.dimensions.content.x,
            row1_col1.dimensions.content.x
        );
        assert!(row1_col0.dimensions.content.y > row0_col0.dimensions.content.y);
    }

    #[test]
    fn flattens_tbody_rows_into_the_table_directly() {
        // <tbody>'s only job at layout time is to be transparent —
        // rows under it become first-class table children. Without that
        // transparency, the table would see one "row" (the tbody) with
        // no cells and bail out at column_count == 0.
        let styled = styled_root(
            r#"<table>
                <tbody>
                    <tr><td><img width="30" height="20"></td></tr>
                    <tr><td><img width="50" height="20"></td></tr>
                </tbody>
            </table>"#,
            r#"table { border-spacing: 0; } td { padding: 0; }"#,
        );
        let layout = layout_tree(&styled, 400.0);
        assert_eq!(layout.children.len(), 2);
        // Column max is 50, so both cells share that width.
        assert_eq!(layout.children[0].dimensions.content.width, 50.0);
        assert_eq!(layout.children[1].dimensions.content.width, 50.0);
    }

    #[test]
    fn merges_thead_and_tbody_rows_in_document_order() {
        // <thead> and <tbody> are both row groups, both transparent.
        // Their <tr> children land in the table's row sequence in
        // document order, which means thead rows appear before tbody
        // rows. Column max-content sees every cell across all groups.
        let styled = styled_root(
            r#"<table>
                <thead><tr><td><img width="20" height="20"></td></tr></thead>
                <tbody>
                    <tr><td><img width="40" height="20"></td></tr>
                    <tr><td><img width="60" height="20"></td></tr>
                </tbody>
            </table>"#,
            r#"table { border-spacing: 0; } td { padding: 0; }"#,
        );
        let layout = layout_tree(&styled, 400.0);
        assert_eq!(layout.children.len(), 3);
        // Column 0 max-content = max(20, 40, 60) = 60.
        for cell in &layout.children {
            assert_eq!(cell.dimensions.content.width, 60.0);
        }
    }

    #[test]
    fn applies_border_spacing_around_and_between_cells() {
        // cellspacing=10 (presentational hint → border-spacing: 10px)
        // adds 10 px gap before the first cell, between adjacent cells,
        // and after the last cell. The test verifies the first two; the
        // trailing edge spacing is implicit in the table's content
        // height/width but not asserted here.
        let styled = styled_root(
            r#"<table cellspacing="10"><tr><td><img width="40" height="20"></td><td><img width="40" height="20"></td></tr></table>"#,
            r#"td { padding: 0; }"#,
        );
        let layout = layout_tree(&styled, 400.0);
        let table_x = layout.dimensions.content.x;
        let cell0 = &layout.children[0];
        let cell1 = &layout.children[1];
        assert_eq!(cell0.dimensions.content.x, table_x + 10.0);
        assert_eq!(
            cell1.dimensions.content.x,
            cell0.dimensions.content.x + 40.0 + 10.0
        );
    }

    #[test]
    fn grows_short_cells_to_match_the_tallest_cell_in_the_row() {
        // The tallest cell drives the row height, and every other cell
        // in that row gets its content height grown to match. Without
        // this, per-cell backgrounds and borders would paint as ragged
        // strips inside what should be a uniform row.
        let styled = styled_root(
            r#"<table><tr><td><img width="40" height="100"></td><td><img width="40" height="20"></td></tr></table>"#,
            r#"table { border-spacing: 0; } td { padding: 0; }"#,
        );
        let layout = layout_tree(&styled, 400.0);
        let cell0 = &layout.children[0];
        let cell1 = &layout.children[1];
        // Cell 0 already at 100 (its image height); cell 1 grew from 20
        // to 100. Both should report the same content height.
        assert_eq!(cell0.dimensions.content.height, 100.0);
        assert_eq!(cell1.dimensions.content.height, 100.0);
    }

    #[test]
    fn colspan_lets_a_cell_span_multiple_columns() {
        // Spanning row: a single `colspan="2"` cell should span the full
        // width of two columns plus the border-spacing between them.
        // Reference row: two single-column cells set the column widths
        // (60 and 40). The spanning cell's outer width = 60 + 40 = 100
        // when border-spacing is zero.
        let styled = styled_root(
            r#"<table>
                <tr><td><img width="60" height="20"></td><td><img width="40" height="20"></td></tr>
                <tr><td colspan="2"><img width="20" height="20"></td></tr>
            </table>"#,
            r#"table { border-spacing: 0; } td { padding: 0; }"#,
        );
        let layout = layout_tree(&styled, 400.0);
        // 3 cells laid out: row0 col0, row0 col1, row1 colspan-2.
        assert_eq!(layout.children.len(), 3);
        let row0_col0 = &layout.children[0];
        let row0_col1 = &layout.children[1];
        let span_cell = &layout.children[2];
        assert_eq!(row0_col0.dimensions.content.width, 60.0);
        assert_eq!(row0_col1.dimensions.content.width, 40.0);
        // Spanning cell occupies columns 0..2, so its outer width is the
        // sum (no inner border-spacing because spacing is 0).
        assert_eq!(span_cell.dimensions.content.width, 100.0);
        // It also starts at the same x as column 0.
        assert_eq!(
            span_cell.dimensions.content.x,
            row0_col0.dimensions.content.x
        );
    }

    #[test]
    fn colspan_aligns_subsequent_cells_to_the_right_column() {
        // HN's subtext row pattern: a `colspan="2"` placeholder eats up
        // the rank + vote-arrow columns so the subtext cell lands in
        // column 2 directly under the title cell. Without colspan
        // support, the subtext cell would slide left into column 1 and
        // never align with the title column above it.
        let styled = styled_root(
            r#"<table>
                <tr><td><img width="20" height="20"></td><td><img width="20" height="20"></td><td><img width="80" height="20"></td></tr>
                <tr><td colspan="2"></td><td><img width="80" height="20"></td></tr>
            </table>"#,
            r#"table { border-spacing: 0; } td { padding: 0; }"#,
        );
        let layout = layout_tree(&styled, 400.0);
        // 4 cells: row0 has 3, row1 has 2 (the colspan-2 + the subtext).
        assert_eq!(layout.children.len(), 5);
        let row0_col2 = &layout.children[2];
        let row1_subtext = &layout.children[4];
        // Title and subtext share the same column-2 x-coordinate.
        assert_eq!(
            row1_subtext.dimensions.content.x,
            row0_col2.dimensions.content.x,
            "subtext must align with title column under its colspan-2 placeholder"
        );
        assert_eq!(row1_subtext.dimensions.content.width, 80.0);
    }

    #[test]
    fn colspan_includes_inner_border_spacing_in_the_spanned_width() {
        // With non-zero border-spacing, a `colspan="2"` cell spans the
        // two columns AND the gap between them. So if both columns are
        // 40 wide and spacing is 10, the spanning cell's outer width
        // is 40 + 10 + 40 = 90.
        let styled = styled_root(
            r#"<table cellspacing="10">
                <tr><td><img width="40" height="20"></td><td><img width="40" height="20"></td></tr>
                <tr><td colspan="2"><img width="20" height="20"></td></tr>
            </table>"#,
            r#"td { padding: 0; }"#,
        );
        let layout = layout_tree(&styled, 400.0);
        let span_cell = &layout.children[2];
        assert_eq!(span_cell.dimensions.content.width, 90.0);
    }

    #[test]
    fn drops_hidden_rows_and_cells_from_the_layout_tree() {
        // display:none is honoured at row and cell granularity. A hidden
        // row never reaches the cell harvest step, so it doesn't push
        // following rows down or contribute to column width measurement.
        let styled = styled_root(
            r#"<table>
                <tr><td><img width="30" height="20"></td><td><img width="30" height="20"></td></tr>
                <tr class="hidden"><td><img width="500" height="20"></td><td><img width="500" height="20"></td></tr>
                <tr><td><img width="40" height="20"></td><td><img width="40" height="20"></td></tr>
            </table>"#,
            r#"table { border-spacing: 0; } td { padding: 0; } .hidden { display: none; }"#,
        );
        let layout = layout_tree(&styled, 400.0);
        // Two visible rows × two columns = 4 cells. The hidden row's
        // 500px cells should not influence column widths.
        assert_eq!(layout.children.len(), 4);
        for cell in &layout.children {
            // Column max-content (across visible rows only) is 40, not 500.
            assert_eq!(cell.dimensions.content.width, 40.0);
        }
    }
}

