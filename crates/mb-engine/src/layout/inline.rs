// Inline / inline-block flow. Inline atoms are laid out left-to-right with
// optional shrink-to-fit width; inline-block boxes get full block layout
// inside but participate in the surrounding inline run.

use crate::{
    css::{Unit, Value},
    dom::NodeType,
    style::StyledNode,
};

use super::block::layout_node;
use super::flex::{is_flex_container, layout_flex_children};
use super::grid::{is_grid_container, layout_grid_children};
use super::{
    BoxType, Dimensions, LayoutBox, Rect, apply_relative_offset, child_height, clamp_height,
    clamp_width, collapsed_text, container_box_type, containing_block_height, content_width_value,
    definite_height, edge_sizes, has_float, in_intrinsic_probe, intrinsic_height, intrinsic_width,
    is_display_none, is_float_right, is_out_of_flow, length_value, outer_rect,
    with_containing_block_height,
};

pub(super) fn layout_inline_children(
    children: &[StyledNode],
    content_x: f32,
    content_y: f32,
    content_width: f32,
    align: InlineAlign,
) -> (Vec<LayoutBox>, f32) {
    // First pass: pack children into lines using their measured widths so we can know
    // each line's total width before placing any box. The placement pass uses that
    // information to offset the line for non-left alignments. Percent-based widths on
    // inline children resolve against `content_width`, which is the parent's content box.
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut line_widths: Vec<f32> = Vec::new();
    let mut current_line: Vec<usize> = Vec::new();
    let mut current_width: f32 = 0.0;

    for (idx, child) in children.iter().enumerate() {
        // `display: none` removes the child entirely — no width contribution,
        // no slot in the second-pass placement. Skipped before the out-of-flow
        // check so a hidden absolute also drops out.
        if is_display_none(child) {
            continue;
        }
        // Absolute children are out of flow — they neither contribute to line
        // width nor cause line breaks. They get laid out separately below.
        if is_out_of_flow(child) {
            continue;
        }
        // Floats are out of flow for line packing too. They sit at the named
        // edge of the container at the current cursor and surrounding inline
        // content runs around them (Tufte-sidenote pattern). The MVP places
        // the float at the parent's content_y without shrinking the line
        // width — overlap on the right side of long lines is left for a
        // follow-up sub-phase; the immediate goal is just to stop floats
        // from being inline-packed at the line's text-width offset.
        if has_float(child) {
            continue;
        }
        let child_w = inline_total_size(child, content_width).width;
        if !current_line.is_empty() && current_width + child_w > content_width {
            lines.push(std::mem::take(&mut current_line));
            line_widths.push(current_width);
            current_width = 0.0;
        }
        current_line.push(idx);
        current_width += child_w;
    }
    if !current_line.is_empty() {
        lines.push(current_line);
        line_widths.push(current_width);
    }

    // Second pass: place each line at its alignment-corrected offset. We read the
    // actual outer height from the laid-out box (rather than re-measuring) so
    // inline-block children, whose auto height is only known after their own
    // layout pass, contribute the right line height here.
    let mut boxes = Vec::new();
    let mut line_y = content_y;
    let mut max_bottom = content_y;

    for (line_idx, line_children) in lines.iter().enumerate() {
        let line_width = line_widths[line_idx];
        let line_offset = match align {
            InlineAlign::Left => 0.0,
            InlineAlign::Center => ((content_width - line_width) / 2.0).max(0.0),
            InlineAlign::Right => (content_width - line_width).max(0.0),
        };
        let mut line_x = content_x + line_offset;
        let mut line_height = 0.0f32;
        for &child_idx in line_children {
            let child = &children[child_idx];
            let mut child_box = layout_inline_or_inline_block(child, line_x, line_y, content_width);
            let outer = outer_rect(&child_box);
            line_x += outer.width;
            line_height = line_height.max(outer.height);
            // `vertical-align: super`/`sub` shifts the child's whole
            // subtree up or down so super/subscript glyphs sit off the
            // baseline. The shift is folded in here (rather than during
            // the child's own layout) so line packing measures the
            // unshifted box for line-height — moving sup up doesn't
            // shrink the parent's line box, matching what Chrome does
            // for typographic super/subscript.
            if let Some(dy) = vertical_align_shift_dy(child) {
                super::shift_layout_subtree(&mut child_box, 0.0, dy);
            }
            boxes.push(child_box);
        }
        max_bottom = max_bottom.max(line_y + line_height);
        line_y += line_height;
    }

    // Lay out absolute children at the parent's content origin (their static
    // position approximation). Pass 2 will replace this with the offsets
    // resolved against their containing block.
    for child in children
        .iter()
        .filter(|child| is_out_of_flow(child) && !is_display_none(child))
    {
        let abs_box = layout_out_of_flow(child, content_x, content_y, content_width);
        boxes.push(abs_box);
    }

    // Float children: place at the named edge of the container at content_y.
    // We lay each float out via the block algorithm at a throwaway cursor —
    // siblings already finished line packing without it, so the float just
    // sits visually beside them. Right floats are measured at the left first
    // and shifted into place; left floats land at content_x directly. The
    // MVP doesn't shrink the affected line widths against the float's
    // outer rect; on long lines this can leave the rightmost word visually
    // overlapping the float. Real Tufte sidenotes use `margin-right: -60%`
    // to escape the parent's content box anyway, which sidesteps the
    // overlap entirely; getting them onto the right Y is the actual fix.
    let mut max_float_bottom = content_y;
    for child in children
        .iter()
        .filter(|child| has_float(child) && !is_display_none(child))
    {
        let mut throwaway = content_y;
        let mut float_box = layout_node(child, content_x, &mut throwaway, content_width);
        if is_float_right(child) {
            let outer_w = outer_rect(&float_box).width;
            let dx = (content_x + content_width - outer_w) - content_x;
            if dx != 0.0 {
                super::shift_layout_subtree(&mut float_box, dx, 0.0);
            }
        }
        max_float_bottom = max_float_bottom.max(throwaway);
        boxes.push(float_box);
    }

    // The container needs to grow to enclose its tallest float too —
    // otherwise `<p>text<aside style="float: right; height: 200px">…</aside>
    // </p>` would close at the inline run's bottom and the float would
    // visually spill below the paragraph background.
    let height = (max_bottom - content_y).max(max_float_bottom - content_y);
    (boxes, height)
}

/// An absolutely positioned / fixed child at its static position (pass 2
/// moves it). Such boxes are blockified and sized shrink-to-fit (CSS 2.1
/// §9.7, §10.3.7) — the inline-block path — whatever their `display`, so
/// their children resolve percentages against the box itself (step 1p).
/// Laying a `display: block` one out as an inline box gave its children the
/// container's width and no definite height.
pub(super) fn layout_out_of_flow(
    node: &StyledNode,
    x: f32,
    y: f32,
    available_width: f32,
) -> LayoutBox {
    if matches!(node.node_type, NodeType::Text(_)) {
        return layout_inline_or_inline_block(node, x, y, available_width);
    }
    layout_inline_block_node(node, x, y, available_width)
}

pub(super) fn layout_inline_or_inline_block(
    node: &StyledNode,
    x: f32,
    y: f32,
    available_width: f32,
) -> LayoutBox {
    if is_inline_block(node) {
        layout_inline_block_node(node, x, y, available_width)
    } else {
        layout_inline_node(node, x, y, available_width)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InlineAlign {
    Left,
    Center,
    Right,
}

pub(super) fn inline_align_for(node: &StyledNode) -> InlineAlign {
    match node.value("text-align") {
        Some(Value::Keyword(keyword)) if keyword == "center" => InlineAlign::Center,
        Some(Value::Keyword(keyword)) if keyword == "right" => InlineAlign::Right,
        _ => InlineAlign::Left,
    }
}

fn layout_inline_node(node: &StyledNode, x: f32, y: f32, parent_width: f32) -> LayoutBox {
    let margin = edge_sizes(node, "margin", parent_width);
    let padding = edge_sizes(node, "padding", parent_width);
    let border = edge_sizes(node, "border", parent_width);
    let (content_width, content_height) = inline_content_size(node, parent_width);
    let content_x = x + margin.left + border.left + padding.left;
    let content_y = y + margin.top + border.top + padding.top;

    // Nested inline children are positioned relative to their inline parent's content box,
    // honoring text-align so labels inside an inline element (e.g. <a class="tile">) can
    // be centered instead of always sticking to the left edge.
    let children = if matches!(&node.node_type, NodeType::Element(element) if element.tag_name != "img")
    {
        let align = inline_align_for(node);
        layout_inline_sequence_no_wrap(
            &node.children,
            content_x,
            content_y,
            content_width,
            parent_width,
            align,
        )
    } else {
        Vec::new()
    };

    let mut layout_box = LayoutBox {
        box_type: BoxType::BlockNode(node.clone()),
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
    };
    apply_relative_offset(&mut layout_box, node, parent_width);
    layout_box
}

/// Lays out an inline box's children on one line. `content_width` is the
/// box's own width (for `text-align` slack); `containing_width` is the
/// width of the nearest block container — the base for the children's
/// percentages and their wrap limit, because an inline box is not a
/// containing block (CSS 2.1 §10.1). Passing the inline box's own
/// content-sized width there made nested `padding: 50%` grow level by
/// level (compat round 3 fuzzer).
fn layout_inline_sequence_no_wrap(
    children: &[StyledNode],
    content_x: f32,
    y: f32,
    content_width: f32,
    containing_width: f32,
    align: InlineAlign,
) -> Vec<LayoutBox> {
    // Sum widths first so we know how much horizontal slack the line has
    // before placing boxes. Absolute children sit out of flow so they don't
    // contribute to the line, and `display: none` children don't exist as
    // far as layout is concerned.
    let total_width: f32 = children
        .iter()
        .filter(|child| !is_out_of_flow(child) && !is_display_none(child))
        .map(|child| inline_total_size(child, containing_width).width)
        .sum();
    let line_offset = match align {
        InlineAlign::Left => 0.0,
        InlineAlign::Center => ((content_width - total_width) / 2.0).max(0.0),
        InlineAlign::Right => (content_width - total_width).max(0.0),
    };

    let mut cursor_x = content_x + line_offset;
    let mut boxes = Vec::new();

    for child in children {
        if is_display_none(child) {
            continue;
        }
        if is_out_of_flow(child) {
            // Static position approximation: at the parent's content origin.
            // Pass 2 will move it once the containing block is known.
            let abs_box = layout_out_of_flow(child, content_x, y, containing_width);
            boxes.push(abs_box);
            continue;
        }
        let child_box = layout_inline_or_inline_block(child, cursor_x, y, containing_width);
        cursor_x += outer_rect(&child_box).width;
        boxes.push(child_box);
    }

    boxes
}

pub(super) fn uses_inline_flow(node: &StyledNode) -> bool {
    // Inline flow only kicks in when all visible **in-flow** children are
    // inline-ish. `display: none` siblings are invisible to flow detection —
    // a hidden <script> alongside inline text should not flip the parent
    // into block flow. Floats are skipped too: spec-wise a float is
    // out-of-flow for line packing, so a Tufte-style `<p>text<aside
    // style="float: right">note</aside>more text</p>` should still be
    // treated as inline flow even though `<aside>` defaults to block.
    // Without this filter the parent would route through block layout and
    // anonymously block-wrap each text run, which makes the surrounding
    // copy stack vertically instead of wrapping around the float.
    let mut visible = node
        .children
        .iter()
        .filter(|child| !is_display_none(child) && !has_float(child))
        .peekable();
    visible.peek().is_some() && visible.all(is_inline_node)
}

/// One in-flow slot of a block-flow parent: either a child laid out on its
/// own, or a run of consecutive inline-level children that share anonymous
/// line boxes (CSS 2.1 §9.2.1.1 — inline content beside block siblings is
/// wrapped in an anonymous block).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BlockChild {
    Single(usize),
    InlineRun(std::ops::Range<usize>),
}

/// Splits a block-flow parent's children into [`BlockChild`] slots, in
/// document order. Whitespace-only text at the edges of a run is trimmed and
/// an all-whitespace run is dropped (inter-element whitespace between block
/// siblings renders as nothing); whitespace *inside* a run stays so words
/// keep their separating spaces. `display: none` children never start a run
/// and are dropped; inside a run they are kept (inline layout skips them).
/// Floats and absolute/fixed children are not inline-level: they end a run
/// and stay `Single` so the block algorithm places them.
pub(super) fn group_block_children(children: &[StyledNode]) -> Vec<BlockChild> {
    let is_run_member =
        |child: &StyledNode| is_inline_node(child) && !has_float(child) && !is_out_of_flow(child);
    let mut groups = Vec::new();
    let mut i = 0;
    while i < children.len() {
        let child = &children[i];
        if is_display_none(child) {
            i += 1;
            continue;
        }
        if !is_run_member(child) {
            groups.push(BlockChild::Single(i));
            i += 1;
            continue;
        }
        let start = i;
        while i < children.len() && (is_run_member(&children[i]) || is_display_none(&children[i])) {
            i += 1;
        }
        let skippable = |c: &StyledNode| super::is_layout_whitespace_text(c) || is_display_none(c);
        let mut run = start..i;
        while run.start < run.end && skippable(&children[run.start]) {
            run.start += 1;
        }
        while run.end > run.start && skippable(&children[run.end - 1]) {
            run.end -= 1;
        }
        if !run.is_empty() {
            groups.push(BlockChild::InlineRun(run));
        }
    }
    groups
}

/// Lays out an anonymous block holding one inline run: line boxes packed at
/// `(x, y)` across `width`. Returns the boxes (flattened — there is no
/// anonymous wrapper in the layout tree, so layout children stay 1:1 with
/// styled children) and the run's height.
pub(super) fn layout_inline_run(
    run: &[StyledNode],
    x: f32,
    y: f32,
    width: f32,
    align: InlineAlign,
) -> (Vec<LayoutBox>, f32) {
    layout_inline_children(run, x, y, width, align)
}

pub(super) fn is_inline_node(node: &StyledNode) -> bool {
    match node.value("display") {
        Some(Value::Keyword(keyword)) if keyword == "block" => return false,
        // inline-block participates in inline flow (sits on a line) but is sized
        // and laid out internally like a block — that dispatch happens later.
        Some(Value::Keyword(keyword)) if keyword == "inline" || keyword == "inline-block" => {
            return true;
        }
        _ => {}
    }

    match &node.node_type {
        NodeType::Text(_) => true,
        // The inline set covers HTML5 "phrasing content" — the tags that
        // real pages mix freely with text inside paragraphs, headings, and
        // table cells. The full phrasing-content list is much larger; this
        // subset is the one that actually appears on the pages we test
        // against (HN, Wikipedia, blog posts). The crucial entries beyond
        // the obvious `a`/`span`/`img` are the text-styling tags
        // (b, i, em, strong, small, mark) — without them, a snippet like
        // `<span><b>name</b> hello</span>` falls into block flow and
        // stacks each child vertically, which is the failure mode that
        // turned the HN nav bar into a vertical list.
        NodeType::Element(element) => matches!(
            element.tag_name.as_str(),
            "a" | "abbr"
                | "b"
                | "button"
                | "input"
                | "select"
                | "textarea"
                | "bdi"
                | "bdo"
                | "big"
                | "br"
                | "cite"
                | "code"
                | "del"
                | "dfn"
                | "em"
                | "font"
                | "i"
                | "img"
                | "ins"
                | "kbd"
                | "label"
                | "mark"
                | "nobr"
                | "q"
                | "s"
                | "samp"
                | "small"
                | "span"
                | "strong"
                | "sub"
                | "sup"
                | "svg"
                | "time"
                | "tt"
                | "u"
                | "var"
                | "wbr"
        ),
    }
}

fn is_inline_block(node: &StyledNode) -> bool {
    matches!(node.value("display"), Some(Value::Keyword(keyword)) if keyword == "inline-block")
}

/// Keyed by node, available width, containing-block height and whether an
/// intrinsic-size probe is running (`%` padding differs there — F1b).
type InlineSizeMemo = std::collections::HashMap<(crate::dom::NodeId, u32, u32, bool), Rect>;

thread_local! {
    /// Inline outer sizes already computed in the current layout pass, by
    /// (DOM node, available width bits). `None` outside a pass.
    static INLINE_SIZE_MEMO: std::cell::RefCell<Option<InlineSizeMemo>> =
        const { std::cell::RefCell::new(None) };
}

/// Run one layout pass with inline sizes memoised (compat round 3 R1).
///
/// Every inline box re-summed its whole subtree to find its own size, and
/// each descendant box did it again — O(n × inline depth); 10 000 nested
/// `<span>`s (flattened to depth 256) took 7.8 s in release.
/// `inline_total_size` is a pure function of the styled node and the
/// available width, so one pass can reuse each answer.
///
/// Keyed by the DOM `NodeId`, not the `StyledNode`'s address: layout boxes
/// and taffy boundary leaves hold *clones* of styled subtrees (same id,
/// same values — layout never edits styles), and the boundary clones are
/// freed mid-pass, so an address could be reused by a different node. A
/// nested pass (another tree, same ids) gets its own memo; the outer one is
/// restored afterwards, also on a panic. Lookup only — the map's order
/// never leaks (determinism).
pub(super) fn with_inline_size_memo<R>(pass: impl FnOnce() -> R) -> R {
    struct Restore(Option<InlineSizeMemo>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let outer = self.0.take();
            INLINE_SIZE_MEMO.with(|memo| *memo.borrow_mut() = outer);
        }
    }
    let outer = INLINE_SIZE_MEMO.with(|memo| memo.borrow_mut().replace(InlineSizeMemo::new()));
    let _restore = Restore(outer);
    pass()
}

fn inline_total_size(node: &StyledNode, parent_width: f32) -> Rect {
    // Percent heights make the size depend on the containing block's
    // height too (step 1p).
    let cb_height = containing_block_height().map_or(u32::MAX, f32::to_bits);
    let key = (
        node.node_id,
        parent_width.to_bits(),
        cb_height,
        in_intrinsic_probe(),
    );
    let hit =
        INLINE_SIZE_MEMO.with(|memo| memo.borrow().as_ref().and_then(|m| m.get(&key).copied()));
    if let Some(size) = hit {
        return size;
    }
    let size = compute_inline_total_size(node, parent_width);
    INLINE_SIZE_MEMO.with(|memo| {
        if let Some(m) = memo.borrow_mut().as_mut() {
            m.insert(key, size);
        }
    });
    size
}

#[cfg(test)]
thread_local! {
    /// How many times an inline outer size was actually computed (not
    /// served from the per-pass memo) — for the R1 complexity test.
    pub(super) static INLINE_SIZE_COMPUTATIONS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

fn compute_inline_total_size(node: &StyledNode, parent_width: f32) -> Rect {
    #[cfg(test)]
    INLINE_SIZE_COMPUTATIONS.with(|c| c.set(c.get() + 1));
    if is_inline_block(node) {
        return inline_block_outer_size(node, parent_width);
    }
    let margin = edge_sizes(node, "margin", parent_width);
    let padding = edge_sizes(node, "padding", parent_width);
    let border = edge_sizes(node, "border", parent_width);
    let (content_width, content_height) = inline_content_size(node, parent_width);
    let width = margin.left
        + border.left
        + padding.left
        + content_width
        + padding.right
        + border.right
        + margin.right;
    let height = margin.top
        + border.top
        + padding.top
        + content_height
        + padding.bottom
        + border.bottom
        + margin.bottom;

    Rect {
        x: 0.0,
        y: 0.0,
        width,
        height,
    }
}

fn inline_block_outer_size(node: &StyledNode, available_width: f32) -> Rect {
    // Line packing only reads the width; the height feeds an enclosing
    // inline box's content height (`inline_content_size`). A definite
    // `height` wins; otherwise the box's own inline content height — through
    // the same memoised measurement, so still one visit per (node, width).
    // It used to be 0, which an inline box sized only by inline-block
    // children (GitHub's breadcrumb `a > span`) came out 0 tall from — hidden
    // until step 1p stopped resolving its `height: 100%` against the width.
    let margin = edge_sizes(node, "margin", available_width);
    let padding = edge_sizes(node, "padding", available_width);
    let border = edge_sizes(node, "border", available_width);
    let content_width = inline_block_resolved_width(node, available_width);
    let content_height = definite_height(node)
        .unwrap_or_else(|| clamp_height(node, inline_content_size(node, content_width).1));
    Rect {
        x: 0.0,
        y: 0.0,
        width: margin.left
            + border.left
            + padding.left
            + content_width
            + padding.right
            + border.right
            + margin.right,
        height: margin.top
            + border.top
            + padding.top
            + content_height
            + padding.bottom
            + border.bottom
            + margin.bottom,
    }
}

/// The content width of an inline-block (also an out-of-flow box or a
/// flex item before flexing): its own width or shrink-to-fit, clamped by
/// min-/max-width (step 5m).
fn inline_block_resolved_width(node: &StyledNode, available_width: f32) -> f32 {
    let width = content_width_value(node, "width", available_width)
        .or_else(|| intrinsic_width(node))
        .unwrap_or_else(|| inline_block_shrink_to_fit_width(node, available_width));
    clamp_width(node, width, available_width)
}

pub(super) fn inline_block_shrink_to_fit_width(node: &StyledNode, available_width: f32) -> f32 {
    // Toy shrink-to-fit: text uses approximate glyph width; element uses the
    // sum of inline child widths, capped to the available content width so a
    // long run still wraps rather than overflowing the container. Floored at
    // 0: negative child margins can make the sum negative, and a wrapper
    // summing several such children multiplied it level by level (compat
    // round 3 fuzzer — `-1e7px` ten levels down). Chrome's intrinsic sizes
    // are clamped the same way.
    let natural = match &node.node_type {
        // CSS-collapsed character count: runs of whitespace render as a
        // single space, so width must be measured against the collapsed
        // form, not the raw source. Same form display_list emits, so the
        // glyph row aligns with the inline box.
        NodeType::Text(text) => measure_inline_text(node, &collapsed_text(node, text)),
        NodeType::Element(_) => node
            .children
            .iter()
            .map(|child| inline_total_size(child, available_width).width)
            .sum(),
    };
    natural.min(available_width).max(0.0)
}

pub(super) fn layout_inline_block_node(
    node: &StyledNode,
    x: f32,
    y: f32,
    available_width: f32,
) -> LayoutBox {
    layout_inline_block_node_as(node, x, y, available_width, false)
}

/// A grid item in its area: like an inline-block, except that with no
/// width of its own it stretches to fill `area_width` (`justify-self:
/// stretch`, the default) instead of shrinking to fit — so its content is
/// laid out at the column's width, not the container's.
pub(super) fn layout_grid_item(node: &StyledNode, x: f32, y: f32, area_width: f32) -> LayoutBox {
    layout_inline_block_node_as(node, x, y, area_width, true)
}

fn layout_inline_block_node_as(
    node: &StyledNode,
    x: f32,
    y: f32,
    available_width: f32,
    stretch: bool,
) -> LayoutBox {
    // Inline-block ignores `margin: auto` (auto only collapses for in-flow
    // blocks), so we just take the raw declared margins here.
    let margin = edge_sizes(node, "margin", available_width);
    let padding = edge_sizes(node, "padding", available_width);
    let border = edge_sizes(node, "border", available_width);
    let stretched = (stretch
        && length_value(node, "width", available_width).is_none()
        && intrinsic_width(node).is_none())
    .then(|| {
        let fill = (available_width
            - margin.left
            - margin.right
            - border.left
            - border.right
            - padding.left
            - padding.right)
            .max(0.0);
        clamp_width(node, fill, available_width)
    });
    let content_width =
        stretched.unwrap_or_else(|| inline_block_resolved_width(node, available_width));

    let content_x = x + margin.left + border.left + padding.left;
    let content_y = y + margin.top + border.top + padding.top;

    // Same dispatch as the regular block path with extra branches for flex
    // and grid containers: dispatch to their respective placement algorithms
    // first; else if every child is inline, run inline flow; otherwise stack
    // block children top-to-bottom inside our content box.
    let own_height = definite_height(node);
    let (children, auto_content_height) = with_containing_block_height(own_height, || {
        if is_flex_container(node) {
            layout_flex_children(node, &node.children, content_x, content_y, content_width)
        } else if is_grid_container(node) {
            layout_grid_children(node, &node.children, content_x, content_y, content_width)
        } else if uses_inline_flow(node) {
            let align = inline_align_for(node);
            layout_inline_children(&node.children, content_x, content_y, content_width, align)
        } else {
            let mut child_cursor_y = content_y;
            let children = node
                .children
                .iter()
                .filter(|child| !is_display_none(child))
                .map(|child| layout_node(child, content_x, &mut child_cursor_y, content_width))
                .collect::<Vec<_>>();
            (children, child_height(node, content_y, child_cursor_y))
        }
    });

    let content_height = own_height
        .unwrap_or_else(|| clamp_height(node, auto_content_height.max(intrinsic_height(node))));

    let mut layout_box = LayoutBox {
        box_type: container_box_type(node),
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
    };
    apply_relative_offset(&mut layout_box, node, available_width);
    layout_box
}

/// Content-box width and height of an inline box. One function for both
/// because both come from the same walk over the children: measuring them
/// separately visited every child twice per nesting level (2^depth on
/// Wikipedia's nested spans).
///
/// Width: text uses `measure_inline_text` (cosmic-text advances when fonts
/// are installed, a `font_size * 0.75` per-char estimate in tests), capped at
/// `parent_width` so a paragraph wider than its container settles into one
/// content box that paint soft-wraps in the same budget. Height: text
/// reserves `line_count * line_height` for that same wrap (one line in the
/// no-fonts test path). Percent heights should reference the parent's
/// height, but only `parent_width` is on hand — toy pages haven't needed it.
fn inline_content_size(node: &StyledNode, parent_width: f32) -> (f32, f32) {
    let declared_width =
        length_value(node, "width", parent_width).or_else(|| intrinsic_width(node));
    let declared_height = definite_height(node);
    if let (Some(width), Some(height)) = (declared_width, declared_height) {
        return (width, height);
    }
    let (width, height) = match &node.node_type {
        NodeType::Text(text) => {
            let text = collapsed_text(node, text);
            let width = match declared_width {
                Some(width) => width,
                None => measure_inline_text(node, &text).min(parent_width),
            };
            let height = match declared_height {
                Some(height) => height,
                None => {
                    let lines = inline_text_line_count(node, &text, parent_width);
                    inline_line_height_px(node) * lines.max(1) as f32
                }
            };
            (width, height)
        }
        NodeType::Element(element) if element.tag_name == "img" => (200.0, intrinsic_height(node)),
        NodeType::Element(_) => {
            let (width, height) = node.children.iter().fold((0.0, 0.0_f32), |(w, h), child| {
                let size = inline_total_size(child, parent_width);
                (w + size.width, h.max(size.height))
            });
            // A size, so never negative: children with negative side
            // margins can sum below 0, and nested wrappers then multiply it
            // (see `inline_block_shrink_to_fit_width`).
            (width.max(0.0), height.max(intrinsic_height(node)))
        }
    };
    (
        declared_width.unwrap_or(width),
        declared_height.unwrap_or(height),
    )
}

fn inline_text_line_count(node: &StyledNode, text: &str, wrap_width: f32) -> u32 {
    if crate::font_system::shared_font_system().is_none() {
        return 1;
    }
    let family = inline_font_family(node);
    crate::render::measure_text_wrap_with_family(
        text,
        inline_font_size(node),
        Some(wrap_width),
        family.as_deref(),
        inline_font_weight(node),
    )
    .1
}

fn inline_font_size(node: &StyledNode) -> f32 {
    // font-size is always Px after the style pass, so the percent base is irrelevant here.
    length_value(node, "font-size", 0.0).unwrap_or(16.0)
}

/// Multiplier used for `line-height: normal` (and unset). The spec lets a UA
/// derive this from font metrics (ascent + descent + line gap), which Chrome
/// and Firefox do per font; 1.2 is the conservative average that proportional
/// body fonts (Helvetica, Arial, Times, system UI) cluster around — close
/// enough that text renders within ~1px per line of Chrome on the pages
/// Phase 6 targets, without plumbing per-font metrics through cosmic-text.
pub(crate) const NORMAL_LINE_HEIGHT_RATIO: f32 = 1.2;

pub(crate) fn inline_line_height_px(node: &StyledNode) -> f32 {
    // CSS `line-height` resolves against the element's *own* font-size:
    // - <number>: bare multiplier (inherits as the number, applied per element)
    // - <length>: absolute (em/rem already converted to Px during style)
    // - <percent>: applied to this node's font-size
    // - keyword `normal` / unset: metric-driven; we approximate with 1.2×.
    let font_size = inline_font_size(node);
    match node.value("line-height") {
        Some(Value::Number(multiplier)) => font_size * multiplier,
        Some(Value::Length(value, Unit::Px)) => *value,
        Some(Value::Length(value, Unit::Percent)) => font_size * value / 100.0,
        Some(Value::Length(value, _)) => *value,
        _ => font_size * NORMAL_LINE_HEIGHT_RATIO,
    }
}

fn inline_char_width(node: &StyledNode) -> f32 {
    inline_font_size(node) * 0.75
}

// Measure the rendered width of `text` at `node`'s font size. When the
// shared cosmic-text FontSystem is installed (the binary at runtime) we
// route through `render::measure_text_width` for shaped advances; the
// uninstalled branch (every unit test that does not call
// `state::install_fonts`) falls back to the legacy character-count ×
// `font_size * 0.75` estimate so layout assertions stay deterministic
// without touching the host's font set.
fn measure_inline_text(node: &StyledNode, text: &str) -> f32 {
    if crate::font_system::shared_font_system().is_none() {
        return text.chars().count() as f32 * inline_char_width(node);
    }
    let family = inline_font_family(node);
    crate::render::measure_text_wrap_with_family(
        text,
        inline_font_size(node),
        None,
        family.as_deref(),
        inline_font_weight(node),
    )
    .0
}

fn inline_font_family(node: &StyledNode) -> Option<String> {
    super::properties::font_family_name(node)
}

/// CSS `vertical-align` shift in pixels relative to the line's baseline.
/// Positive moves down (sub), negative moves up (super); `None` means
/// the child stays at the line's natural top-aligned position. The shift
/// is rooted in the child's own font-size so a sup nested in a 30px
/// heading lifts further than a sup inside 14px body copy. 0.4× the
/// child's font (which is ~0.33× the parent under the `font-size:
/// smaller` ratio) is the conservative offset Chrome and Firefox both
/// land near for footnote markers.
fn vertical_align_shift_dy(node: &StyledNode) -> Option<f32> {
    match node.value("vertical-align") {
        Some(Value::Keyword(keyword)) => match keyword.as_str() {
            "super" => Some(-0.4 * inline_font_size(node)),
            "sub" => Some(0.4 * inline_font_size(node)),
            _ => None,
        },
        _ => None,
    }
}

fn inline_font_weight(node: &StyledNode) -> u16 {
    // Mirrors `display_list::font_weight_value`: keyword bold/normal map to
    // 700/400, numeric values pass through clamped, anything else falls
    // back to the cosmic-text default of 400. Inline measurement consults
    // this so a `<b>` run shapes against the same bold face the paint pass
    // will render, keeping shrink-to-fit widths consistent.
    match node.value("font-weight") {
        Some(Value::Keyword(keyword)) => match keyword.to_ascii_lowercase().as_str() {
            "bold" => 700,
            "normal" => 400,
            _ => 400,
        },
        Some(Value::Number(n)) => (n.round() as i32).clamp(1, 1000) as u16,
        _ => 400,
    }
}
