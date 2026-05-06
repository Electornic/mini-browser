// CSS → taffy::Style converter and the `layout_via_taffy` dispatch entry.
// Em/rem are already resolved to px at style time (see `style.rs`), so we
// only see Px and Percent here. Calc()/min()/max() are not in the AST yet
// and fall through to defaults. Anything we don't understand keeps taffy's
// `Style::DEFAULT` value, which matches CSS initial values for the
// relevant property.
//
// Phase 4.3 final state (post 4.3.1–4.3.5): every element root routes
// through taffy. The dispatch is two-tier:
//   • NATIVE — block, flex (`Display::Flex`), and grid (`Display::Grid`)
//     containers are mapped to taffy nodes via `to_taffy_style` and
//     positioned by taffy directly. The walk-back reads `taffy::Layout`
//     fields back into our `LayoutBox`.
//   • BOUNDARY — inline-flow blocks, tables, floats / clear, out-of-flow
//     subtrees, percent vertical inset/padding, and grid-template-areas
//     containers become opaque taffy leaves (`new_leaf_with_context`
//     carrying a `NodeBoundary`). The measure callback runs the legacy
//     `block::layout_node` once per leaf at the width taffy provides;
//     walk-back splices the cached LayoutBox in after shifting it from
//     origin (0, 0) to the absolute position taffy assigned. The boundary
//     leaf's taffy style retains the node's margin so block-flow margin
//     collapse with native siblings still works.
//
// Legacy block / inline / flex / grid / table layout code remains live
// because the boundary measure callback still drives it for the shapes
// listed above — the legacy algorithms own the semantics taffy doesn't
// support natively.

use taffy::prelude::{
    Dimension, LengthPercentage, LengthPercentageAuto, TaffyGridLine, TaffyGridSpan, TaffyTree,
};
use taffy::style::{
    AlignContent, AlignItems, AlignSelf, AvailableSpace, BoxSizing, Display, FlexDirection,
    FlexWrap, GridAutoFlow, GridPlacement as TaffyGridPlacement, GridTemplateComponent,
    JustifyContent, MaxTrackSizingFunction, MinTrackSizingFunction, Overflow, Position, Style,
    TrackSizingFunction,
};
use taffy::geometry::{Line, MinMax, Rect as TaffyRect, Size};
use taffy::NodeId;

use crate::css::{GridLine, TrackSize, Unit, Value};
use crate::dom::NodeType;
use crate::style::StyledNode;

use super::{
    Dimensions, EdgeSizes, LayoutBox, Rect, container_box_type, has_float, intrinsic_height,
    intrinsic_width, is_display_none, is_layout_whitespace_text, is_out_of_flow, outer_rect,
    shift_layout_subtree,
};
use super::block::layout_node as legacy_block_layout;
use super::inline::uses_inline_flow;
use super::table::is_table_container;

use std::cell::RefCell;

pub fn to_taffy_style(node: &StyledNode) -> Style {
    let mut style = Style::DEFAULT;

    style.display = display_value(node);
    style.box_sizing = box_sizing_value(node);
    style.position = position_value(node);
    style.overflow = overflow_value(node);

    // CSS width/height take precedence over intrinsic size; fall back to the
    // intrinsic helpers (used by the legacy path) when no explicit CSS value is
    // present so img/input/textarea still get their attribute-driven defaults.
    style.size = Size {
        width: dimension_with_intrinsic(node.value("width"), intrinsic_width(node)),
        height: dimension_with_intrinsic(node.value("height"), intrinsic_height_opt(node)),
    };
    style.min_size = Size {
        width: dimension(node.value("min-width")),
        height: dimension(node.value("min-height")),
    };
    style.max_size = Size {
        width: dimension(node.value("max-width")),
        height: dimension(node.value("max-height")),
    };

    style.margin = edge_lpa(node, "margin");
    style.padding = edge_lp(node, "padding");
    style.border = border_widths(node);
    style.inset = TaffyRect {
        left: lpa_or_auto(node.value("left")),
        right: lpa_or_auto(node.value("right")),
        top: lpa_or_auto(node.value("top")),
        bottom: lpa_or_auto(node.value("bottom")),
    };

    style.flex_direction = flex_direction_value(node);
    style.flex_wrap = flex_wrap_value(node);
    style.flex_grow = number_value(node.value("flex-grow")).unwrap_or(0.0);
    style.flex_shrink = number_value(node.value("flex-shrink")).unwrap_or(1.0);
    style.flex_basis = dimension(node.value("flex-basis"));

    style.align_items = align_items_value(node);
    style.align_self = align_self_value(node);
    style.align_content = align_content_value(node);
    style.justify_content = justify_content_value(node);

    style.gap = Size {
        width: lp(node.value("column-gap").or_else(|| node.value("gap"))),
        height: lp(node.value("row-gap").or_else(|| node.value("gap"))),
    };

    style.grid_auto_flow = grid_auto_flow_value(node);
    style.grid_template_columns = grid_template(node.value("grid-template-columns"));
    style.grid_template_rows = grid_template(node.value("grid-template-rows"));
    style.grid_column = grid_line_pair(node.value("grid-column"));
    style.grid_row = grid_line_pair(node.value("grid-row"));

    style
}

fn display_value(node: &StyledNode) -> Display {
    match keyword(node.value("display")) {
        Some("flex") | Some("inline-flex") => Display::Flex,
        Some("grid") | Some("inline-grid") => Display::Grid,
        Some("none") => Display::None,
        Some("block") | Some("inline-block") | Some("list-item") | None => Display::Block,
        // table/table-* fall back to Block; Phase 4.3e wires real table layout.
        _ => Display::Block,
    }
}

fn box_sizing_value(node: &StyledNode) -> BoxSizing {
    match keyword(node.value("box-sizing")) {
        Some("border-box") => BoxSizing::BorderBox,
        _ => BoxSizing::ContentBox,
    }
}

fn position_value(node: &StyledNode) -> Position {
    match keyword(node.value("position")) {
        // Taffy has no `Fixed`; we keep doing the viewport-anchored
        // pass ourselves (`reposition_absolutes`).
        Some("absolute") | Some("fixed") => Position::Absolute,
        _ => Position::Relative,
    }
}

fn overflow_value(node: &StyledNode) -> Point<Overflow> {
    let x = overflow_axis(node.value("overflow-x").or_else(|| node.value("overflow")));
    let y = overflow_axis(node.value("overflow-y").or_else(|| node.value("overflow")));
    Point { x, y }
}

fn overflow_axis(value: Option<&Value>) -> Overflow {
    match keyword(value) {
        Some("hidden") => Overflow::Hidden,
        Some("scroll") | Some("auto") => Overflow::Scroll,
        _ => Overflow::Visible,
    }
}

// taffy 0.10 spells the overflow holder `Point` (x/y), reused here.
type Point<T> = taffy::geometry::Point<T>;

fn flex_direction_value(node: &StyledNode) -> FlexDirection {
    match keyword(node.value("flex-direction")) {
        Some("row-reverse") => FlexDirection::RowReverse,
        Some("column") => FlexDirection::Column,
        Some("column-reverse") => FlexDirection::ColumnReverse,
        _ => FlexDirection::Row,
    }
}

fn flex_wrap_value(node: &StyledNode) -> FlexWrap {
    match keyword(node.value("flex-wrap")) {
        Some("wrap") => FlexWrap::Wrap,
        Some("wrap-reverse") => FlexWrap::WrapReverse,
        _ => FlexWrap::NoWrap,
    }
}

fn align_items_value(node: &StyledNode) -> Option<AlignItems> {
    parse_align_items(keyword(node.value("align-items"))?)
}

fn align_self_value(node: &StyledNode) -> Option<AlignSelf> {
    parse_align_items(keyword(node.value("align-self"))?)
}

fn parse_align_items(kw: &str) -> Option<AlignItems> {
    match kw {
        "flex-start" | "start" => Some(AlignItems::FlexStart),
        "flex-end" | "end" => Some(AlignItems::FlexEnd),
        "center" => Some(AlignItems::Center),
        "baseline" => Some(AlignItems::Baseline),
        "stretch" => Some(AlignItems::Stretch),
        _ => None,
    }
}

fn align_content_value(node: &StyledNode) -> Option<AlignContent> {
    match keyword(node.value("align-content"))? {
        "flex-start" | "start" => Some(AlignContent::FlexStart),
        "flex-end" | "end" => Some(AlignContent::FlexEnd),
        "center" => Some(AlignContent::Center),
        "stretch" => Some(AlignContent::Stretch),
        "space-between" => Some(AlignContent::SpaceBetween),
        "space-around" => Some(AlignContent::SpaceAround),
        "space-evenly" => Some(AlignContent::SpaceEvenly),
        _ => None,
    }
}

fn justify_content_value(node: &StyledNode) -> Option<JustifyContent> {
    match keyword(node.value("justify-content"))? {
        "flex-start" | "start" => Some(JustifyContent::FlexStart),
        "flex-end" | "end" => Some(JustifyContent::FlexEnd),
        "center" => Some(JustifyContent::Center),
        "space-between" => Some(JustifyContent::SpaceBetween),
        "space-around" => Some(JustifyContent::SpaceAround),
        "space-evenly" => Some(JustifyContent::SpaceEvenly),
        _ => None,
    }
}

fn grid_auto_flow_value(node: &StyledNode) -> GridAutoFlow {
    match keyword(node.value("grid-auto-flow")) {
        Some("column") => GridAutoFlow::Column,
        Some("row dense") => GridAutoFlow::RowDense,
        Some("column dense") => GridAutoFlow::ColumnDense,
        _ => GridAutoFlow::Row,
    }
}

// ---------- length / dimension helpers ----------

fn dimension(value: Option<&Value>) -> Dimension {
    match value {
        Some(Value::Length(v, Unit::Px)) => Dimension::length(*v),
        Some(Value::Length(v, Unit::Percent)) => Dimension::percent(*v / 100.0),
        Some(Value::Keyword(kw)) if kw == "auto" => Dimension::auto(),
        _ => Dimension::auto(),
    }
}

fn dimension_with_intrinsic(css: Option<&Value>, intrinsic: Option<f32>) -> Dimension {
    // CSS values always win; the intrinsic fallback only kicks in when CSS
    // would otherwise resolve to `auto`. Used so img/input/textarea pull
    // their attribute-derived size into the taffy Style.
    match css {
        Some(Value::Length(v, Unit::Px)) => Dimension::length(*v),
        Some(Value::Length(v, Unit::Percent)) => Dimension::percent(*v / 100.0),
        _ => match intrinsic {
            Some(v) => Dimension::length(v),
            None => Dimension::auto(),
        },
    }
}

fn intrinsic_height_opt(node: &StyledNode) -> Option<f32> {
    // The legacy `intrinsic_height` returns 0.0 for elements that have no
    // intrinsic height — collapse that to `None` so the bridge keeps Dimension
    // ::auto for plain blocks (otherwise every block would be height-clamped
    // to zero).
    let h = intrinsic_height(node);
    if h > 0.0 { Some(h) } else { None }
}

fn lpa_or_auto(value: Option<&Value>) -> LengthPercentageAuto {
    // For properties whose CSS initial value is `auto` (top/right/bottom/left).
    match value {
        Some(Value::Length(v, Unit::Px)) => LengthPercentageAuto::length(*v),
        Some(Value::Length(v, Unit::Percent)) => LengthPercentageAuto::percent(*v / 100.0),
        Some(Value::Keyword(kw)) if kw == "auto" => LengthPercentageAuto::auto(),
        _ => LengthPercentageAuto::auto(),
    }
}

fn lpa_or_zero(value: Option<&Value>) -> LengthPercentageAuto {
    // For properties whose CSS initial value is 0 (margin sides). The keyword
    // `auto` still maps through when explicitly authored.
    match value {
        Some(Value::Length(v, Unit::Px)) => LengthPercentageAuto::length(*v),
        Some(Value::Length(v, Unit::Percent)) => LengthPercentageAuto::percent(*v / 100.0),
        Some(Value::Keyword(kw)) if kw == "auto" => LengthPercentageAuto::auto(),
        _ => LengthPercentageAuto::length(0.0),
    }
}

fn lp(value: Option<&Value>) -> LengthPercentage {
    match value {
        Some(Value::Length(v, Unit::Px)) => LengthPercentage::length(*v),
        Some(Value::Length(v, Unit::Percent)) => LengthPercentage::percent(*v / 100.0),
        _ => LengthPercentage::length(0.0),
    }
}

fn edge_lpa(node: &StyledNode, prefix: &str) -> TaffyRect<LengthPercentageAuto> {
    // Sole caller is `margin`, whose initial value is 0 (auto only via author).
    TaffyRect {
        left: lpa_or_zero(node.value(&format!("{prefix}-left"))),
        right: lpa_or_zero(node.value(&format!("{prefix}-right"))),
        top: lpa_or_zero(node.value(&format!("{prefix}-top"))),
        bottom: lpa_or_zero(node.value(&format!("{prefix}-bottom"))),
    }
}

fn edge_lp(node: &StyledNode, prefix: &str) -> TaffyRect<LengthPercentage> {
    TaffyRect {
        left: lp(node.value(&format!("{prefix}-left"))),
        right: lp(node.value(&format!("{prefix}-right"))),
        top: lp(node.value(&format!("{prefix}-top"))),
        bottom: lp(node.value(&format!("{prefix}-bottom"))),
    }
}

fn border_widths(node: &StyledNode) -> TaffyRect<LengthPercentage> {
    // Our style pipeline stores the per-side border WIDTH under the shorthand
    // key (`border-left`), not under the longhand (`border-left-width`). The
    // legacy `edge_sizes(node, "border", base)` does the same, so match it.
    TaffyRect {
        left: lp(node.value("border-left")),
        right: lp(node.value("border-right")),
        top: lp(node.value("border-top")),
        bottom: lp(node.value("border-bottom")),
    }
}

fn keyword(value: Option<&Value>) -> Option<&str> {
    match value? {
        Value::Keyword(kw) => Some(kw.as_str()),
        _ => None,
    }
}

fn number_value(value: Option<&Value>) -> Option<f32> {
    match value? {
        Value::Number(n) => Some(*n),
        Value::Length(v, _) => Some(*v),
        _ => None,
    }
}

// ---------- grid helpers ----------

fn grid_template(value: Option<&Value>) -> Vec<GridTemplateComponent<String>> {
    let Some(Value::TrackList(tracks)) = value else {
        return Vec::new();
    };
    tracks
        .iter()
        .map(|track| GridTemplateComponent::Single(track_to_taffy(track)))
        .collect()
}

fn track_to_taffy(track: &TrackSize) -> TrackSizingFunction {
    match track {
        TrackSize::Length(v, Unit::Px) => MinMax {
            min: MinTrackSizingFunction::length(*v),
            max: MaxTrackSizingFunction::length(*v),
        },
        TrackSize::Length(v, Unit::Percent) => MinMax {
            min: MinTrackSizingFunction::percent(*v / 100.0),
            max: MaxTrackSizingFunction::percent(*v / 100.0),
        },
        TrackSize::Length(v, _) => MinMax {
            min: MinTrackSizingFunction::length(*v),
            max: MaxTrackSizingFunction::length(*v),
        },
        TrackSize::Fraction(weight) => MinMax {
            min: MinTrackSizingFunction::auto(),
            max: MaxTrackSizingFunction::fr(*weight),
        },
        TrackSize::Auto => MinMax {
            min: MinTrackSizingFunction::auto(),
            max: MaxTrackSizingFunction::auto(),
        },
    }
}

fn grid_line_pair(value: Option<&Value>) -> Line<TaffyGridPlacement<String>> {
    let Some(Value::GridPlacement(placement)) = value else {
        return Line {
            start: TaffyGridPlacement::Auto,
            end: TaffyGridPlacement::Auto,
        };
    };
    Line {
        start: grid_line_to_taffy(placement.start),
        end: grid_line_to_taffy(placement.end),
    }
}

fn grid_line_to_taffy(line: GridLine) -> TaffyGridPlacement<String> {
    match line {
        GridLine::Auto => TaffyGridPlacement::Auto,
        GridLine::Index(idx) => TaffyGridPlacement::from_line_index(idx as i16),
        GridLine::Span(n) => TaffyGridPlacement::from_span(n as u16),
    }
}

// ---------- Dispatch: native taffy with measure-callback boundaries ----------
//
// `layout_via_taffy` walks the StyledNode tree, builds a parallel taffy
// tree, runs `compute_layout_with_measure`, then walks taffy + StyledNode
// together to reconstitute our `LayoutBox`. See the file-header comment
// for the native vs. boundary split.

struct NodeBoundary {
    node: StyledNode,
    cached: RefCell<Option<LayoutBox>>,
}

pub(super) fn layout_via_taffy(node: &StyledNode, viewport_width: f32) -> Option<LayoutBox> {
    if !matches!(node.node_type, NodeType::Element(_)) {
        return None;
    }
    let mut tree: TaffyTree<NodeBoundary> = TaffyTree::new();
    // Taffy rounds final layouts to integer pixels by default; legacy code
    // ran in f32 throughout, so disable rounding to keep parity with the
    // existing test suite (and so flex-shrink distributions like 66.67,
    // 66.67, 66.67 don't drift to 67, 66, 67).
    tree.disable_rounding();
    let root_id = build_taffy_node(&mut tree, node)?;
    // Wrapper for root `margin: auto` resolution against the viewport (taffy
    // does not auto-center the topmost node it lays out).
    let wrapper_style = Style {
        display: Display::Block,
        size: Size {
            width: Dimension::length(viewport_width),
            height: Dimension::auto(),
        },
        ..Style::DEFAULT
    };
    let wrapper_id = tree.new_with_children(wrapper_style, &[root_id]).ok()?;
    tree.compute_layout_with_measure(
        wrapper_id,
        Size {
            width: AvailableSpace::Definite(viewport_width),
            height: AvailableSpace::MaxContent,
        },
        |_known, avail, _id, ctx, _style| measure_boundary(avail, ctx),
    )
    .ok()?;
    // wrapper sits at (0, 0) and has zero padding/border, so the root's
    // border-box origin in viewport coords is just root.location.
    Some(walk(&tree, root_id, node, (0.0, 0.0)))
}

fn measure_boundary(
    avail: Size<AvailableSpace>,
    ctx: Option<&mut NodeBoundary>,
) -> Size<f32> {
    let Some(boundary) = ctx else {
        return Size::ZERO;
    };
    let parent_width = match avail.width {
        AvailableSpace::Definite(w) => w,
        // Boundary leaves only see MaxContent / MinContent inside flex / grid
        // intrinsic-size probes, which the boundary path does not yet drive
        // (containers themselves still become boundaries in 4.3.1). Pick a
        // big-but-finite proxy for max-content and 0 for min-content so the
        // legacy block algorithm produces sensible widths in both extremes.
        AvailableSpace::MaxContent => 1.0e6,
        AvailableSpace::MinContent => 0.0,
    };
    let mut cursor_y = 0.0;
    let lb = legacy_block_layout(&boundary.node, 0.0, &mut cursor_y, parent_width);
    let outer = outer_rect(&lb);
    let margin_w = lb.dimensions.margin.left + lb.dimensions.margin.right;
    let margin_h = lb.dimensions.margin.top + lb.dimensions.margin.bottom;
    *boundary.cached.borrow_mut() = Some(lb);
    // Return the BORDER-box size; taffy applies the leaf's own margin (set in
    // `boundary_taffy_style`) on top so block-flow margin collapse with native
    // siblings still works.
    Size {
        width: (outer.width - margin_w).max(0.0),
        height: (outer.height - margin_h).max(0.0),
    }
}

fn build_taffy_node(
    tree: &mut TaffyTree<NodeBoundary>,
    node: &StyledNode,
) -> Option<NodeId> {
    if is_native_block(node) {
        let style = to_taffy_style(node);
        let mut children = Vec::new();
        for child in &node.children {
            if is_display_none(child) || is_layout_whitespace_text(child) {
                continue;
            }
            children.push(build_taffy_node(tree, child)?);
        }
        tree.new_with_children(style, &children).ok()
    } else {
        let style = boundary_taffy_style(node);
        tree.new_leaf_with_context(
            style,
            NodeBoundary {
                node: node.clone(),
                cached: RefCell::new(None),
            },
        )
        .ok()
    }
}

fn boundary_taffy_style(node: &StyledNode) -> Style {
    // Boundary leaves participate in the parent's block flow but expose no
    // internal structure to taffy — `size: auto` (the default) lets the
    // measure callback determine the BORDER-box dimensions. The node's
    // margin is preserved so taffy's block algorithm can still collapse it
    // against native sibling margins; padding/border live inside the cached
    // LayoutBox and must NOT be forwarded to taffy or they would be
    // double-counted.
    //
    // `width` / `min-width` / `max-width` ARE forwarded so taffy can clamp
    // the available space the measure callback receives. Without this, a
    // boundary block with `max-width: 65ch` would be sized at the full
    // parent width because the legacy block algorithm doesn't read max-width
    // — the cap only takes effect when taffy applies it before measure runs.
    let mut style = Style::DEFAULT;
    style.display = Display::Block;
    style.margin = edge_lpa(node, "margin");
    style.size = Size {
        width: dimension_with_intrinsic(node.value("width"), intrinsic_width(node)),
        height: Dimension::auto(),
    };
    style.min_size = Size {
        width: dimension(node.value("min-width")),
        height: Dimension::auto(),
    };
    style.max_size = Size {
        width: dimension(node.value("max-width")),
        height: Dimension::auto(),
    };
    style
}

fn is_native_block(node: &StyledNode) -> bool {
    // Self-checks: features that disqualify THIS node from being a native
    // taffy block container (we'd lay it out via legacy as a single boundary
    // leaf instead).
    if !matches!(node.node_type, NodeType::Element(_)) {
        return false;
    }
    // 4.3.2 / 4.3.3: flex AND grid containers are now native — taffy maps
    // `display: flex` / `display: grid` (plus inline variants) and consumes
    // the flex-* / align-* / justify-* / gap fields and the grid-template /
    // grid-row / grid-column placement fields that the bridge already
    // populates. Children that aren't natively supportable (inline-flow,
    // floats, etc.) fall through to the boundary measure-callback path and
    // taffy treats their measured size as the item's contribution to the
    // flex / grid track sizing.
    // 4.3.4: tables intentionally stay on the boundary path. taffy 0.10
    // has no `display: table*` support and CSS table sizing (column-equal,
    // colspan, rowgroups, anonymous box generation for orphan cells) is
    // sufficiently different from CSS Grid that a faithful mapping would
    // re-implement most of `layout/table.rs` inside the bridge. Keeping
    // tables as boundary leaves lets the legacy `layout_table_children`
    // own the semantics, which is correct rather than expedient.
    if is_table_container(node)
        || uses_inline_flow(node)
        || is_out_of_flow(node)
        || has_float(node)
        || has_clear(node)
        || has_percent_inset_or_padding(node)
        || uses_grid_template_areas(node)
    {
        return false;
    }
    // Child-level check: taffy's block algorithm doesn't honor CSS floats /
    // clear or treat absolute/fixed children as out of flow. If any of our
    // children carry those, we must drop down to a boundary leaf so the
    // legacy code can apply the right semantics for the entire subtree.
    // Inline-flow / flex / grid / table CHILDREN are fine — they each become
    // their own boundary leaf, and taffy stacks those opaque rectangles in
    // a perfectly ordinary block flow on our behalf.
    !node
        .children
        .iter()
        .any(|c| has_float(c) || has_clear(c) || is_out_of_flow(c))
}

fn has_clear(node: &StyledNode) -> bool {
    matches!(
        node.value("clear"),
        Some(Value::Keyword(k)) if k == "left" || k == "right" || k == "both"
    )
}

fn uses_grid_template_areas(node: &StyledNode) -> bool {
    // taffy 0.10's grid algorithm has no `grid-template-areas` support, so a
    // container that uses named-area placement falls back to the legacy grid
    // pass via the boundary path. Items reference areas via `grid-area: <name>`,
    // which our css crate stores as a keyword on the item — but the container
    // is the real source of the name table, so detecting it on the container
    // alone is sufficient.
    matches!(node.value("grid-template-areas"), Some(Value::TemplateAreas(_)))
}

fn has_percent_inset_or_padding(node: &StyledNode) -> bool {
    let percent = |key: &str| {
        matches!(node.value(key), Some(Value::Length(_, Unit::Percent)))
    };
    // Legacy resolves percent vertical padding/inset against parent_width;
    // taffy follows the spec. Stay on legacy by treating any such node as a
    // boundary leaf so the quirk is preserved.
    percent("padding-top")
        || percent("padding-bottom")
        || percent("top")
        || percent("bottom")
}

fn walk(
    tree: &TaffyTree<NodeBoundary>,
    id: NodeId,
    node: &StyledNode,
    parent_border_origin: (f32, f32),
) -> LayoutBox {
    let layout = tree.layout(id).expect("layout was just computed");
    let abs_border_x = parent_border_origin.0 + layout.location.x;
    let abs_border_y = parent_border_origin.1 + layout.location.y;

    if is_native_block(node) {
        let content_x = abs_border_x + layout.border.left + layout.padding.left;
        let content_y = abs_border_y + layout.border.top + layout.padding.top;
        let content_width = (layout.size.width
            - layout.border.left
            - layout.border.right
            - layout.padding.left
            - layout.padding.right)
            .max(0.0);
        let content_height = (layout.size.height
            - layout.border.top
            - layout.border.bottom
            - layout.padding.top
            - layout.padding.bottom)
            .max(0.0);

        let dimensions = Dimensions {
            content: Rect {
                x: content_x,
                y: content_y,
                width: content_width,
                height: content_height,
            },
            padding: edge_from_taffy(&layout.padding),
            border: edge_from_taffy(&layout.border),
            margin: edge_from_taffy(&layout.margin),
        };

        let child_ids = tree.children(id).unwrap_or_default();
        let mut child_layouts = Vec::with_capacity(child_ids.len());
        let next_origin = (abs_border_x, abs_border_y);
        let mut taffy_iter = child_ids.into_iter();
        for child in &node.children {
            if is_display_none(child) || is_layout_whitespace_text(child) {
                continue;
            }
            let child_id = match taffy_iter.next() {
                Some(id) => id,
                None => break,
            };
            child_layouts.push(walk(tree, child_id, child, next_origin));
        }

        LayoutBox {
            box_type: container_box_type(node),
            dimensions,
            children: child_layouts,
        }
    } else {
        // Boundary leaf: the cached LayoutBox sits at outer (0, 0). Shift its
        // outer-left to (abs_border_x - margin.left), outer-top similarly,
        // so the border-box lands exactly where taffy positioned the leaf.
        let abs_outer_x = abs_border_x - layout.margin.left;
        let abs_outer_y = abs_border_y - layout.margin.top;
        let ctx = tree
            .get_node_context(id)
            .expect("boundary leaves carry a NodeBoundary context");
        let mut cached = ctx
            .cached
            .borrow_mut()
            .take()
            .unwrap_or_else(|| {
                // Defensive: if the measure callback never fired (taffy may
                // skip when known dimensions are already definite), run
                // legacy here using taffy's resolved leaf size as the width.
                let mut cursor_y = 0.0;
                legacy_block_layout(&ctx.node, 0.0, &mut cursor_y, layout.size.width)
            });
        shift_layout_subtree(&mut cached, abs_outer_x, abs_outer_y);
        cached
    }
}

fn edge_from_taffy(rect: &TaffyRect<f32>) -> EdgeSizes {
    EdgeSizes {
        left: rect.left,
        right: rect.right,
        top: rect.top,
        bottom: rect.bottom,
    }
}

#[cfg(test)]
mod tests {
    use super::layout_via_taffy;
    use crate::{css, html, layout::layout_tree, style};

    fn styled_root(html_source: &str, css_source: &str) -> style::StyledNode {
        let document = html::parse(html_source).unwrap();
        let root = document.roots()[0];
        let stylesheet = css::parse(css_source).unwrap();
        style::style_tree(&document, root, &[stylesheet])
    }

    #[test]
    fn taffy_matches_legacy_for_leaf_block() {
        // Single empty <div> with explicit width/height/margin/padding/border.
        // The taffy bridge should produce the same content rect and edge
        // sizes as the legacy block algorithm.
        let styled = styled_root(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 120px;
                    height: 60px;
                    margin: 8px;
                    padding: 6px;
                    border: 2px solid black;
                }
            "#,
        );

        let legacy = layout_tree(&styled, 800.0);
        let bridged = layout_via_taffy(&styled, 800.0)
            .expect("leaf block must be supported in 4.3b");

        assert_eq!(legacy.dimensions.content, bridged.dimensions.content);
        assert_eq!(legacy.dimensions.padding, bridged.dimensions.padding);
        assert_eq!(legacy.dimensions.border, bridged.dimensions.border);
        assert_eq!(legacy.dimensions.margin, bridged.dimensions.margin);
    }

    #[test]
    fn taffy_routes_inline_flow_subtree_through_boundary() {
        // 4.3.1: a block parent with an inline-flow `<p>` child stays native
        // at the parent level; the `<p>` becomes a boundary leaf measured by
        // the legacy block algorithm. The bridged output must match legacy.
        let styled = styled_root(
            r#"<div id="root"><p>One</p></div>"#,
            r#"#root { width: 300px; } p { font-size: 16px; }"#,
        );
        let legacy = layout_tree(&styled, 800.0);
        let bridged = layout_via_taffy(&styled, 800.0)
            .expect("element root always routes through taffy after 4.3.1");
        assert_eq!(legacy.dimensions.content, bridged.dimensions.content);
        assert_eq!(legacy.children.len(), bridged.children.len());
        assert_eq!(legacy.children[0].dimensions.content, bridged.children[0].dimensions.content);
    }

    #[test]
    fn taffy_routes_table_through_boundary() {
        // 4.3.4: tables are deliberately a boundary case — verify the
        // bridge produces output identical to the legacy table layout for a
        // simple 2x2 table with explicit cell content widths.
        let styled = styled_root(
            r#"<table id="t">
                <tr><td class="c"></td><td class="c"></td></tr>
                <tr><td class="c"></td><td class="c"></td></tr>
            </table>"#,
            r#"
                #t { display: table; width: 200px; }
                tr { display: table-row; }
                td.c { display: table-cell; width: 100px; height: 30px; }
            "#,
        );
        let legacy = layout_tree(&styled, 800.0);
        let bridged = layout_via_taffy(&styled, 800.0)
            .expect("element root always routes through taffy");
        assert_eq!(legacy.dimensions.content, bridged.dimensions.content);
        assert_eq!(legacy.children.len(), bridged.children.len());
    }

    #[test]
    fn taffy_handles_relative_position_with_px_offsets() {
        // 4.3e: position:relative with non-percent inset now routes through
        // taffy. taffy positions the box statically and shifts by inset —
        // same end result as the legacy post-pass.
        let styled = styled_root(
            r#"<div id="root"><div class="box"></div></div>"#,
            r#"
                #root { width: 200px; }
                .box { position: relative; left: 30px; top: 12px; height: 10px; }
            "#,
        );
        let legacy = layout_tree(&styled, 800.0);
        let bridged = layout_via_taffy(&styled, 800.0)
            .expect("relative + px inset must be supported in 4.3e");
        assert_eq!(legacy.children[0].dimensions.content, bridged.children[0].dimensions.content);
    }

    #[test]
    fn taffy_handles_nested_block_subtree() {
        // 4.3d: empty nested blocks (no text) route through taffy and
        // produce identical geometry to the legacy block algorithm.
        let styled = styled_root(
            r#"<div id="outer"><div class="inner"></div><div class="inner"></div></div>"#,
            r#"
                #outer { width: 200px; padding: 10px; }
                .inner { height: 30px; margin: 5px 0; border: 1px solid black; }
            "#,
        );
        let legacy = layout_tree(&styled, 800.0);
        let bridged = layout_via_taffy(&styled, 800.0)
            .expect("nested block subtree must be supported in 4.3d");

        assert_eq!(legacy.dimensions.content, bridged.dimensions.content);
        assert_eq!(legacy.children.len(), bridged.children.len());
        for (a, b) in legacy.children.iter().zip(bridged.children.iter()) {
            assert_eq!(a.dimensions.content, b.dimensions.content);
            assert_eq!(a.dimensions.margin, b.dimensions.margin);
            assert_eq!(a.dimensions.border, b.dimensions.border);
        }
    }
}
