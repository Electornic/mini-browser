// CSS Flexbox layout — single-axis grow/shrink, justify-content, align-items.

use crate::{css::Value, style::StyledNode};

use super::inline::{layout_inline_block_node, layout_out_of_flow};
use super::{
    LayoutBox, clamp_height, clamp_width, containing_block_height, has_height_limits,
    has_width_limits, is_display_none, is_layout_whitespace_text, is_out_of_flow, length_like,
    outer_rect, shift_layout_subtree,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlexDirection {
    Row,
    Column,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JustifyContent {
    FlexStart,
    Center,
    FlexEnd,
    SpaceBetween,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlignItems {
    Stretch,
    FlexStart,
    Center,
    FlexEnd,
}

pub(super) fn layout_flex_children(
    container: &StyledNode,
    children: &[StyledNode],
    content_x: f32,
    content_y: f32,
    content_width: f32,
) -> (Vec<LayoutBox>, f32) {
    // Two-pass placement. Pass 1: lay out every in-flow item at the container's
    // content origin so we can measure each item's outer main/cross size
    // without committing to a final position. Pass 2: read justify-content
    // (main axis) and align-items (cross axis), shift each item by its
    // computed offset on each axis. For align-items: stretch, items without
    // an explicit cross size also have their content cross size grown to fill
    // the container.
    //
    // Sizing comes from the inline-block path (explicit width wins, otherwise
    // shrink-to-fit). Margin collapse and floats are skipped on flex items
    // per spec.
    let direction = flex_direction(container);
    let justify = justify_content(container);
    let align = align_items(container);

    let mut boxes: Vec<LayoutBox> = Vec::with_capacity(children.len());
    // Track (boxes index, source styled node) for each in-flow item so pass 2
    // can read the styled node again to decide whether stretch should grow
    // the item.
    let mut in_flow: Vec<(usize, &StyledNode)> = Vec::new();

    for child in children {
        if is_display_none(child) {
            continue;
        }
        // Inter-element whitespace text nodes (preserved by the HTML
        // parser to keep inline runs spaced) must not become flex
        // items — they would steal a slot in the main-axis packing
        // and break grow/shrink/align math.
        if is_layout_whitespace_text(child) {
            continue;
        }
        if is_out_of_flow(child) {
            // Static-position approximation, same trick as inline flow: drop
            // the absolute child at the container's content origin and let
            // the absolute reposition pass at the tree root move it.
            let abs_box = layout_out_of_flow(child, content_x, content_y, content_width);
            boxes.push(abs_box);
            continue;
        }
        in_flow.push((boxes.len(), child));
        boxes.push(layout_flex_item(child, content_x, content_y, content_width));
    }

    // Apply explicit `flex-basis: <length>` as a content-axis override on
    // top of pass-1 sizing. CSS spec: flex-basis takes precedence over the
    // item's `width`/`height` along the main axis, so the override happens
    // after pass 1 even though it would also have been valid to substitute
    // the basis upstream.
    // A percentage basis resolves against the container's main size: the
    // content width for a row, its definite height for a column (`None` →
    // the basis behaves as `content`, i.e. auto) — step 1b, `flex: 1` is
    // `1 1 0%`.
    let main_size = match direction {
        FlexDirection::Row => Some(content_width),
        FlexDirection::Column => containing_block_height(),
    };
    for &(i, child) in &in_flow {
        if let Some(basis) = flex_basis_content_main(child, main_size) {
            force_item_content_main(&mut boxes[i], basis, direction);
        }
    }

    let total_basis: f32 = in_flow
        .iter()
        .map(|&(i, _)| main_axis_outer(&boxes[i], direction))
        .sum();

    // Container's main-axis content size. Row direction is anchored to the
    // already-resolved content_width. Column direction needs an explicit
    // height for `justify-content`/grow to have any leftover to distribute —
    // height: auto falls back to total_basis so free space is zero.
    let container_main_size = match direction {
        FlexDirection::Row => content_width,
        // Inside our child layout the installed containing-block height
        // is this container's own definite height (step 1p).
        FlexDirection::Column => containing_block_height().unwrap_or(total_basis),
    };

    // Distribute free space along the main axis. Positive free space goes to
    // flex-grow (proportional to grow weight). Negative free space goes to
    // flex-shrink (proportional to shrink weight × basis, per spec, so larger
    // items absorb more shrinkage). Items with zero weight are skipped on
    // their respective passes.
    let free = container_main_size - total_basis;
    let limited = in_flow.iter().any(|&(_, child)| match direction {
        FlexDirection::Row => has_width_limits(child),
        FlexDirection::Column => has_height_limits(child),
    });
    if limited {
        // Step 5m: an item's min-/max- main size takes part in flexing.
        // Unlimited rows keep the single distribution below unchanged.
        resolve_limited_flex(
            &mut boxes,
            &in_flow,
            direction,
            container_main_size,
            content_width,
        );
    } else if free > 0.0 {
        let total_grow: f32 = in_flow.iter().map(|&(_, child)| flex_grow(child)).sum();
        if total_grow > 0.0 {
            for &(i, child) in &in_flow {
                let grow = flex_grow(child);
                if grow > 0.0 {
                    let delta = free * grow / total_grow;
                    grow_item_main(&mut boxes[i], delta, direction);
                }
            }
        }
    } else if free < 0.0 {
        let total_shrink_weight: f32 = in_flow
            .iter()
            .map(|&(i, child)| flex_shrink(child) * main_axis_outer(&boxes[i], direction))
            .sum();
        if total_shrink_weight > 0.0 {
            for &(i, child) in &in_flow {
                let weight = flex_shrink(child) * main_axis_outer(&boxes[i], direction);
                if weight > 0.0 {
                    // free is negative; delta is the (negative) size delta to
                    // apply to this item, so larger weights shrink more.
                    let delta = free * weight / total_shrink_weight;
                    grow_item_main(&mut boxes[i], delta, direction);
                }
            }
        }
    }

    // After grow/shrink, total_used reflects the actually-occupied main size.
    // When grow consumed all positive free space (or shrink absorbed all
    // negative free space) the leftover is zero and justify-content has
    // nothing to distribute.
    let total_used: f32 = in_flow
        .iter()
        .map(|&(i, _)| main_axis_outer(&boxes[i], direction))
        .sum();
    let leftover_main = (container_main_size - total_used).max(0.0);
    let item_count = in_flow.len();

    let (start_offset, between_gap) = match justify {
        JustifyContent::FlexStart => (0.0, 0.0),
        JustifyContent::Center => (leftover_main / 2.0, 0.0),
        JustifyContent::FlexEnd => (leftover_main, 0.0),
        JustifyContent::SpaceBetween if item_count > 1 => {
            (0.0, leftover_main / (item_count - 1) as f32)
        }
        // Single-item space-between collapses to flex-start (no gap to distribute).
        JustifyContent::SpaceBetween => (0.0, 0.0),
    };

    // Container's cross size — needed before pass 2 so each item knows what
    // to align against. For row direction, height may be explicit or fall back
    // to the tallest item's outer cross size; for column direction the cross
    // axis is width, which is always already resolved.
    let max_cross_natural: f32 = in_flow
        .iter()
        .map(|&(i, _)| cross_axis_outer(&boxes[i], direction))
        .fold(0.0, f32::max);
    let container_cross_size = match direction {
        FlexDirection::Row => containing_block_height().unwrap_or(max_cross_natural),
        FlexDirection::Column => content_width,
    };

    let mut cursor = start_offset;
    for (idx_in_flow, &(i, child)) in in_flow.iter().enumerate() {
        // Stretch grows the item's content cross size to fill the container,
        // but only when the item didn't declare its own cross size — explicit
        // sizes always win over stretch per spec. The growth happens before
        // we read cross_axis_outer below so the post-stretch height feeds the
        // alignment math correctly.
        if matches!(align, AlignItems::Stretch) && !has_explicit_cross_size(child, direction) {
            stretch_item_to_cross(&mut boxes[i], container_cross_size, direction);
        }

        let main_size = main_axis_outer(&boxes[i], direction);
        let cross_size = cross_axis_outer(&boxes[i], direction);
        let cross_offset = match align {
            AlignItems::FlexStart | AlignItems::Stretch => 0.0,
            AlignItems::Center => ((container_cross_size - cross_size) / 2.0).max(0.0),
            AlignItems::FlexEnd => (container_cross_size - cross_size).max(0.0),
        };

        match direction {
            FlexDirection::Row => shift_layout_subtree(&mut boxes[i], cursor, cross_offset),
            FlexDirection::Column => shift_layout_subtree(&mut boxes[i], cross_offset, cursor),
        }
        cursor += main_size;
        if idx_in_flow + 1 < item_count {
            cursor += between_gap;
        }
    }

    // Auto height for the container depends on direction:
    // - row:    cross axis = height, so it grows to the tallest item
    //           (post-stretch). When the container has explicit height, that
    //           wins anyway in layout_node — this fallback only matters in the
    //           auto case, where container_cross_size == max_cross_natural.
    // - column: main axis  = height, so it grows to the cumulative cursor.
    let auto_content_height = match direction {
        FlexDirection::Row => container_cross_size,
        FlexDirection::Column => cursor,
    };
    (boxes, auto_content_height)
}

/// Resolve flexible lengths with min/max constraints (CSS Flexbox §9.7):
/// distribute the free space, clamp each item, freeze the items whose
/// clamp moved them the way the total violation points (or all of them
/// when it is 0), and distribute again among the rest. At most one round
/// per item. Weights are the same as the unlimited path (grow factor;
/// shrink factor × outer base size).
fn resolve_limited_flex(
    boxes: &mut [LayoutBox],
    in_flow: &[(usize, &StyledNode)],
    direction: FlexDirection,
    container_main: f32,
    content_width: f32,
) {
    let clamp = |child: &StyledNode, size: f32| match direction {
        FlexDirection::Row => clamp_width(child, size, content_width),
        FlexDirection::Column => clamp_height(child, size),
    };
    let content_main = |b: &LayoutBox| match direction {
        FlexDirection::Row => b.dimensions.content.width,
        FlexDirection::Column => b.dimensions.content.height,
    };
    let items: Vec<(f32, f32)> = in_flow
        .iter()
        .map(|&(i, _)| {
            let base = content_main(&boxes[i]);
            (base, main_axis_outer(&boxes[i], direction) - base)
        })
        .collect();
    let hypothetical: f32 = in_flow
        .iter()
        .zip(&items)
        .map(|(&(_, child), &(base, edges))| clamp(child, base) + edges)
        .sum();
    let growing = container_main > hypothetical;
    let factor = |child: &StyledNode| {
        if growing {
            flex_grow(child)
        } else {
            flex_shrink(child)
        }
    };
    // Inflexible items sit at their hypothetical size from the start.
    let mut target: Vec<f32> = Vec::with_capacity(items.len());
    let mut frozen: Vec<bool> = Vec::with_capacity(items.len());
    for (&(_, child), &(base, _)) in in_flow.iter().zip(&items) {
        let hypothetical = clamp(child, base);
        let inflexible = factor(child) <= 0.0
            || (growing && base > hypothetical)
            || (!growing && base < hypothetical);
        target.push(if inflexible { hypothetical } else { base });
        frozen.push(inflexible);
    }
    for _ in 0..=items.len() {
        if frozen.iter().all(|&f| f) {
            break;
        }
        let used: f32 = items
            .iter()
            .zip(&target)
            .zip(&frozen)
            .map(|((&(base, edges), &t), &f)| if f { t } else { base } + edges)
            .sum();
        let free = container_main - used;
        let weight = |k: usize, child: &StyledNode| {
            if growing {
                flex_grow(child)
            } else {
                flex_shrink(child) * (items[k].0 + items[k].1)
            }
        };
        let total_weight: f32 = (0..items.len())
            .filter(|&k| !frozen[k])
            .map(|k| weight(k, in_flow[k].1))
            .sum();
        let mut violation = 0.0;
        let mut clamped = vec![0.0_f32; items.len()];
        for k in (0..items.len()).filter(|&k| !frozen[k]) {
            let child = in_flow[k].1;
            let share = if total_weight > 0.0 {
                free * weight(k, child) / total_weight
            } else {
                0.0
            };
            let unclamped = (items[k].0 + share).max(0.0);
            target[k] = clamp(child, unclamped);
            clamped[k] = target[k] - unclamped;
            violation += clamped[k];
        }
        for k in 0..items.len() {
            frozen[k] = frozen[k]
                || violation == 0.0
                || (violation > 0.0 && clamped[k] > 0.0)
                || (violation < 0.0 && clamped[k] < 0.0);
        }
    }
    for (&(i, _), &t) in in_flow.iter().zip(&target) {
        force_item_content_main(&mut boxes[i], t, direction);
    }
}

fn stretch_item_to_cross(
    layout_box: &mut LayoutBox,
    container_cross_size: f32,
    direction: FlexDirection,
) {
    // Grow the item's content rect on the cross axis so its outer size matches
    // the container's cross. Margins/borders/padding stay as declared, so the
    // delta lands on content size only. Children that already laid out inside
    // do not move — they stay at their original positions and any gained
    // space appears as background area at the trailing edge, which is the
    // simplest reasonable visual approximation of stretch for a toy renderer.
    let outer = outer_rect(layout_box);
    let current_outer_cross = match direction {
        FlexDirection::Row => outer.height,
        FlexDirection::Column => outer.width,
    };
    if current_outer_cross >= container_cross_size {
        return;
    }
    let delta = container_cross_size - current_outer_cross;
    match direction {
        FlexDirection::Row => layout_box.dimensions.content.height += delta,
        FlexDirection::Column => layout_box.dimensions.content.width += delta,
    }
}

fn align_items(node: &StyledNode) -> AlignItems {
    match node.value("align-items") {
        Some(Value::Keyword(keyword)) if keyword == "flex-start" => AlignItems::FlexStart,
        Some(Value::Keyword(keyword)) if keyword == "center" => AlignItems::Center,
        Some(Value::Keyword(keyword)) if keyword == "flex-end" => AlignItems::FlexEnd,
        Some(Value::Keyword(keyword)) if keyword == "stretch" => AlignItems::Stretch,
        // CSS default for align-items is `stretch`.
        _ => AlignItems::Stretch,
    }
}

fn has_explicit_cross_size(node: &StyledNode, direction: FlexDirection) -> bool {
    let prop = match direction {
        FlexDirection::Row => "height",
        FlexDirection::Column => "width",
    };
    length_like(node, prop).is_some()
}

fn flex_grow(node: &StyledNode) -> f32 {
    // CSS default is 0 — items don't grow unless the author opts in.
    match node.value("flex-grow") {
        Some(Value::Number(value)) if *value >= 0.0 => *value,
        _ => 0.0,
    }
}

fn flex_shrink(node: &StyledNode) -> f32 {
    // CSS default is 1 — items shrink to fit by default.
    match node.value("flex-shrink") {
        Some(Value::Number(value)) if *value >= 0.0 => *value,
        _ => 1.0,
    }
}

fn flex_basis_content_main(node: &StyledNode, main_size: Option<f32>) -> Option<f32> {
    // A length or a resolvable percentage drives the content-axis
    // override; `auto` / `content` leave layout_flex_item's width /
    // shrink-to-fit logic in charge.
    // A percentage (or a calc with one) is of the main size, and is `auto`
    // when that is indefinite.
    match length_like(node, "flex-basis")? {
        length if length.has_percent() => main_size.map(|size| length.resolve(size)),
        length => Some(length.resolve(0.0)),
    }
}

fn force_item_content_main(
    layout_box: &mut LayoutBox,
    content_main: f32,
    direction: FlexDirection,
) {
    let value = content_main.max(0.0);
    match direction {
        FlexDirection::Row => layout_box.dimensions.content.width = value,
        FlexDirection::Column => layout_box.dimensions.content.height = value,
    }
}

fn grow_item_main(layout_box: &mut LayoutBox, delta: f32, direction: FlexDirection) {
    // Resize is post-hoc on the item's content rect — children that were laid
    // out in pass 1 keep their original positions, so any extra space appears
    // as background area at the trailing edge. This is the same simplification
    // already used by stretch_item_to_cross.
    match direction {
        FlexDirection::Row => {
            layout_box.dimensions.content.width =
                (layout_box.dimensions.content.width + delta).max(0.0);
        }
        FlexDirection::Column => {
            layout_box.dimensions.content.height =
                (layout_box.dimensions.content.height + delta).max(0.0);
        }
    }
}

fn main_axis_outer(layout_box: &LayoutBox, direction: FlexDirection) -> f32 {
    let outer = outer_rect(layout_box);
    match direction {
        FlexDirection::Row => outer.width,
        FlexDirection::Column => outer.height,
    }
}

fn cross_axis_outer(layout_box: &LayoutBox, direction: FlexDirection) -> f32 {
    let outer = outer_rect(layout_box);
    match direction {
        FlexDirection::Row => outer.height,
        FlexDirection::Column => outer.width,
    }
}

pub(super) fn layout_flex_item(
    node: &StyledNode,
    x: f32,
    y: f32,
    available_width: f32,
) -> LayoutBox {
    // Flex items are block-level boxes from the inside (their own children
    // still lay out as block/inline/flex), but on the outside they get
    // shrink-to-fit sizing rather than stretching to fill the parent. The
    // inline-block path already implements exactly that sizing rule, so we
    // reuse it directly.
    layout_inline_block_node(node, x, y, available_width)
}

pub(super) fn is_flex_container(node: &StyledNode) -> bool {
    matches!(node.value("display"), Some(Value::Keyword(keyword)) if keyword == "flex")
}

fn flex_direction(node: &StyledNode) -> FlexDirection {
    match node.value("flex-direction") {
        Some(Value::Keyword(keyword)) if keyword == "column" => FlexDirection::Column,
        _ => FlexDirection::Row,
    }
}

fn justify_content(node: &StyledNode) -> JustifyContent {
    match node.value("justify-content") {
        Some(Value::Keyword(keyword)) if keyword == "center" => JustifyContent::Center,
        Some(Value::Keyword(keyword)) if keyword == "flex-end" => JustifyContent::FlexEnd,
        Some(Value::Keyword(keyword)) if keyword == "space-between" => JustifyContent::SpaceBetween,
        _ => JustifyContent::FlexStart,
    }
}
