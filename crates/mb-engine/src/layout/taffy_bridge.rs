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

use taffy::NodeId;
use taffy::geometry::{Line, MinMax, Rect as TaffyRect, Size};
use taffy::prelude::{
    Dimension, LengthPercentage, LengthPercentageAuto, TaffyGridLine, TaffyGridSpan, TaffyTree,
};
use taffy::style::{
    AlignContent, AlignItems, AlignSelf, AvailableSpace, BoxSizing, Display, FlexDirection,
    FlexWrap, GridAutoFlow, GridPlacement as TaffyGridPlacement, GridTemplateComponent,
    JustifyContent, MaxTrackSizingFunction, MinTrackSizingFunction, Overflow, Position, Style,
    TrackSizingFunction,
};

use crate::css::{GridLine, TrackBreadth, TrackSize, Unit, Value};
use crate::dom::NodeType;
use crate::style::StyledNode;

use super::block::layout_node as legacy_block_layout;
use super::inline::{
    BlockChild, InlineAlign, group_block_children, inline_align_for, layout_inline_run,
    uses_inline_flow,
};
use super::table::is_table_container;
use super::{
    BoxType, Dimensions, EdgeSizes, LayoutBox, Rect, container_box_type, has_float,
    intrinsic_height, intrinsic_width, is_display_none, is_layout_whitespace_text, is_out_of_flow,
    length_value, outer_rect, shift_layout_subtree, with_containing_block_height,
    with_intrinsic_probe,
};

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
        width: lpa_or_auto(node.value("min-width")),
        height: lpa_or_auto(node.value("min-height")),
    };
    style.max_size = Size {
        width: lpa_or_auto(node.value("max-width")),
        height: lpa_or_auto(node.value("max-height")),
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

    style.align_items = align_items_value(node).unwrap_or(AlignItems::NORMAL);
    style.align_self = align_self_value(node);
    style.align_content = align_content_value(node).unwrap_or(AlignContent::NORMAL);
    style.justify_content = justify_content_value(node).unwrap_or(JustifyContent::NORMAL);

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
        "flex-start" | "start" => Some(AlignItems::FLEX_START),
        "flex-end" | "end" => Some(AlignItems::FLEX_END),
        "center" => Some(AlignItems::CENTER),
        "baseline" => Some(AlignItems::BASELINE),
        "stretch" => Some(AlignItems::STRETCH),
        _ => None,
    }
}

fn align_content_value(node: &StyledNode) -> Option<AlignContent> {
    match keyword(node.value("align-content"))? {
        "flex-start" | "start" => Some(AlignContent::FLEX_START),
        "flex-end" | "end" => Some(AlignContent::FLEX_END),
        "center" => Some(AlignContent::CENTER),
        "stretch" => Some(AlignContent::STRETCH),
        "space-between" => Some(AlignContent::SPACE_BETWEEN),
        "space-around" => Some(AlignContent::SPACE_AROUND),
        "space-evenly" => Some(AlignContent::SPACE_EVENLY),
        _ => None,
    }
}

fn justify_content_value(node: &StyledNode) -> Option<JustifyContent> {
    match keyword(node.value("justify-content"))? {
        "flex-start" | "start" => Some(JustifyContent::FLEX_START),
        "flex-end" | "end" => Some(JustifyContent::FLEX_END),
        "center" => Some(JustifyContent::CENTER),
        "space-between" => Some(JustifyContent::SPACE_BETWEEN),
        "space-around" => Some(JustifyContent::SPACE_AROUND),
        "space-evenly" => Some(JustifyContent::SPACE_EVENLY),
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
        TrackSize::MinContent => MinMax {
            min: MinTrackSizingFunction::min_content(),
            max: MaxTrackSizingFunction::min_content(),
        },
        TrackSize::MaxContent => MinMax {
            min: MinTrackSizingFunction::max_content(),
            max: MaxTrackSizingFunction::max_content(),
        },
        TrackSize::MinMax(min, max) => MinMax {
            min: min_breadth_to_taffy(min),
            max: max_breadth_to_taffy(max),
        },
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

fn min_breadth_to_taffy(breadth: &TrackBreadth) -> MinTrackSizingFunction {
    match breadth {
        TrackBreadth::Length(v, Unit::Percent) => MinTrackSizingFunction::percent(*v / 100.0),
        TrackBreadth::Length(v, _) => MinTrackSizingFunction::length(*v),
        TrackBreadth::MinContent => MinTrackSizingFunction::min_content(),
        TrackBreadth::MaxContent => MinTrackSizingFunction::max_content(),
        // `fr` never parses as a minimum; `auto` either way.
        TrackBreadth::Auto | TrackBreadth::Fraction(_) => MinTrackSizingFunction::auto(),
    }
}

fn max_breadth_to_taffy(breadth: &TrackBreadth) -> MaxTrackSizingFunction {
    match breadth {
        TrackBreadth::Length(v, Unit::Percent) => MaxTrackSizingFunction::percent(*v / 100.0),
        TrackBreadth::Length(v, _) => MaxTrackSizingFunction::length(*v),
        TrackBreadth::Fraction(weight) => MaxTrackSizingFunction::fr(*weight),
        TrackBreadth::MinContent => MaxTrackSizingFunction::min_content(),
        TrackBreadth::MaxContent => MaxTrackSizingFunction::max_content(),
        TrackBreadth::Auto => MaxTrackSizingFunction::auto(),
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
    content: BoundaryContent,
    cached: RefCell<Option<LayoutBox>>,
}

enum BoundaryContent {
    /// A whole subtree the legacy block algorithm lays out.
    Node(StyledNode),
    /// An anonymous block: consecutive inline-level children of a block
    /// container that has block children too. Cached as an `AnonymousBlock`
    /// box whose children `walk` splices into the parent (flattened).
    InlineRun {
        children: Vec<StyledNode>,
        align: InlineAlign,
    },
}

impl NodeBoundary {
    fn new(content: BoundaryContent) -> Self {
        Self {
            content,
            cached: RefCell::new(None),
        }
    }

    /// The boundary's own left + right margin as the legacy layout will
    /// resolve it (`auto` → 0: an auto margin only absorbs leftover space,
    /// which taffy already gave away). An anonymous inline run has none.
    fn horizontal_margins(&self, percent_base: f32) -> f32 {
        match &self.content {
            BoundaryContent::Node(node) => {
                length_value(node, "margin-left", percent_base).unwrap_or(0.0)
                    + length_value(node, "margin-right", percent_base).unwrap_or(0.0)
            }
            BoundaryContent::InlineRun { .. } => 0.0,
        }
    }

    fn layout(&self, parent_width: f32) -> LayoutBox {
        match &self.content {
            BoundaryContent::Node(node) => {
                let mut cursor_y = 0.0;
                legacy_block_layout(node, 0.0, &mut cursor_y, parent_width)
            }
            BoundaryContent::InlineRun { children, align } => {
                let (boxes, height) = layout_inline_run(children, 0.0, 0.0, parent_width, *align);
                LayoutBox {
                    box_type: BoxType::AnonymousBlock,
                    dimensions: Dimensions {
                        content: Rect {
                            x: 0.0,
                            y: 0.0,
                            width: parent_width,
                            height,
                        },
                        ..Dimensions::default()
                    },
                    children: boxes,
                }
            }
        }
    }
}

/// `viewport_height` is the initial containing block's height — what the
/// root's `height: N%` resolves against (`html, body {height: 100%}`).
/// `None` leaves it indefinite (callers without a viewport height).
pub(super) fn layout_via_taffy(
    node: &StyledNode,
    viewport_width: f32,
    viewport_height: Option<f32>,
) -> Option<LayoutBox> {
    if !matches!(node.node_type, NodeType::Element(_)) {
        return None;
    }
    let mut tree: TaffyTree<NodeBoundary> = TaffyTree::new();
    // Taffy rounds final layouts to integer pixels by default; legacy code
    // ran in f32 throughout, so disable rounding to keep parity with the
    // existing test suite (and so flex-shrink distributions like 66.67,
    // 66.67, 66.67 don't drift to 67, 66, 67).
    tree.disable_rounding();
    // Reset the per-layout measure-callback accumulators before kicking
    // off taffy. The OnceLock-gated print at the end reports how many
    // times our `legacy_block_layout` boundary callback was invoked and
    // how long it spent, so we can separate taffy's own solver cost from
    // our text-shaping cost inside the measure path.
    MEASURE_CALLS.with(|c| c.set(0));
    MEASURE_NANOS.with(|n| n.set(0));
    let root_id =
        mb_dom::profile::time_block("taffy::build_tree", || build_taffy_node(&mut tree, node))?;
    // The document element establishes the initial block formatting
    // context: its margins never collapse with its children's (CSS 2.1
    // 8.3.1), so `<body>`'s margin stays inside `<html>` and the page height
    // includes it, as in Chrome. Taffy only stops the collapse at a scroll
    // container; the flag is layout-only (paint reads the CSS `overflow`).
    if matches!(&node.node_type, NodeType::Element(e) if e.tag_name.eq_ignore_ascii_case("html"))
        && let Ok(style) = tree.style(root_id)
    {
        let mut style = style.clone();
        style.overflow = Point {
            x: Overflow::Hidden,
            y: Overflow::Hidden,
        };
        let _ = tree.set_style(root_id, style);
    }
    // Wrapper for root `margin: auto` resolution against the viewport (taffy
    // does not auto-center the topmost node it lays out).
    // Its definite height is the initial containing block's (step 1p): the
    // root's percentage height resolves against it, and taffy hands it on to
    // boundary leaves as `parent_size.height`. The root's own box is still
    // its content height — the wrapper is never part of the layout tree.
    let wrapper_style = Style {
        display: Display::Block,
        size: Size {
            width: Dimension::length(viewport_width),
            height: viewport_height.map_or(Dimension::auto(), Dimension::length),
        },
        ..Style::DEFAULT
    };
    let wrapper_id = tree.new_with_children(wrapper_style, &[root_id]).ok()?;
    mb_dom::profile::time_block("taffy::compute_layout", || {
        tree.compute_layout_with_measure(
            wrapper_id,
            Size {
                width: AvailableSpace::Definite(viewport_width),
                height: AvailableSpace::MaxContent,
            },
            |inputs, _id, ctx, style| {
                taffy::compute_leaf_layout(
                    inputs,
                    style,
                    |_, _| 0.0,
                    |_known, avail| measure_boundary(avail, inputs.parent_size, ctx),
                )
            },
        )
        .ok()
    })?;
    if mb_dom::profile::enabled() {
        let calls = MEASURE_CALLS.with(|c| c.get());
        let nanos = MEASURE_NANOS.with(|n| n.get());
        eprintln!(
            "[mb-profile] taffy::measure_callback: calls={calls} total={:.2}ms",
            nanos as f64 / 1_000_000.0
        );
    }
    // wrapper sits at (0, 0) and has zero padding/border, so the root's
    // border-box origin in viewport coords is just root.location.
    Some(mb_dom::profile::time_block("taffy::walk_result", || {
        walk(&tree, root_id, node, (0.0, 0.0))
    }))
}

// Per-layout measure-callback accumulators. thread_local because taffy
// itself is single-threaded for one `compute_layout` invocation; making
// them globals would conflate concurrent test runs.
std::thread_local! {
    static MEASURE_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static MEASURE_NANOS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// `containing_width` is taffy's parent size (the base for percentage
/// margins), when it knows it.
/// `containing` is the leaf's containing block as taffy knows it: the
/// width for margins, and the height — `Some` only when definite — that the
/// legacy layout resolves percentage heights against (step 1p).
fn measure_boundary(
    avail: Size<AvailableSpace>,
    containing: Size<Option<f32>>,
    ctx: Option<&mut NodeBoundary>,
) -> Size<f32> {
    let containing_width = containing.width;
    let Some(boundary) = ctx else {
        return Size::ZERO;
    };
    let _measure_start = std::time::Instant::now();
    let parent_width = match avail.width {
        // taffy gives a leaf the space for its BORDER box — the leaf's own
        // margins (set in `boundary_taffy_style`) are already subtracted.
        // The legacy layout below resolves and subtracts those margins
        // itself, so hand it the margin-box width; otherwise every boundary
        // leaf with side margins lost them twice (a `<body>` of inline
        // content was 768px in an 800px viewport; compat round 3 R2).
        AvailableSpace::Definite(w) => {
            w + boundary.horizontal_margins(containing_width.unwrap_or(w))
        }
        // Boundary leaves only see MaxContent / MinContent inside flex / grid
        // intrinsic-size probes, which the boundary path does not yet drive
        // (containers themselves still become boundaries in 4.3.1). Pick a
        // big-but-finite proxy for max-content and 0 for min-content so the
        // legacy block algorithm produces sensible widths in both extremes.
        AvailableSpace::MaxContent => 1.0e6,
        AvailableSpace::MinContent => 0.0,
    };
    let probe = !matches!(avail.width, AvailableSpace::Definite(_));
    let mut lb = with_intrinsic_probe(probe, || {
        with_containing_block_height(containing.height, || boundary.layout(parent_width))
    });
    if !matches!(avail.width, AvailableSpace::Definite(_))
        && probe_sizes_to_lines(&boundary.content)
    {
        // Intrinsic-size probe: an anonymous block's — or an auto-width
        // element's whose children are all inline — min/max-content width
        // is its widest line, not the 0 / 1e6 proxy it was packed against.
        // Reporting the proxy made every such flex item's basis 1e6 px:
        // shrinking split the row evenly instead of by content, and with
        // `flex-shrink: 0` the item stayed 1e6 px wide (compat round 3
        // fuzzer). Other boundary shapes (tables, floats …) still report
        // what the legacy layout gave.
        // The lines' span, not their right edge: `text-align: center` /
        // `right` puts them in the middle / at the end of the proxy width.
        let (left, right) = lb
            .children
            .iter()
            .map(outer_rect)
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(l, r), outer| {
                (l.min(outer.x), r.max(outer.x + outer.width))
            });
        lb.dimensions.content.width = (right - left).max(0.0);
    }
    let outer = outer_rect(&lb);
    let margin_w = lb.dimensions.margin.left + lb.dimensions.margin.right;
    let margin_h = lb.dimensions.margin.top + lb.dimensions.margin.bottom;
    *boundary.cached.borrow_mut() = Some(lb);
    MEASURE_CALLS.with(|c| c.set(c.get() + 1));
    MEASURE_NANOS.with(|n| n.set(n.get() + _measure_start.elapsed().as_nanos() as u64));
    // Return the BORDER-box size; taffy applies the leaf's own margin (set in
    // `boundary_taffy_style`) on top so block-flow margin collapse with native
    // siblings still works.
    Size {
        width: (outer.width - margin_w).max(0.0),
        height: (outer.height - margin_h).max(0.0),
    }
}

/// Whether an intrinsic-size probe of this boundary should measure its laid
/// out lines instead of trusting the proxy width (see `measure_boundary`).
fn probe_sizes_to_lines(content: &BoundaryContent) -> bool {
    match content {
        BoundaryContent::InlineRun { .. } => true,
        BoundaryContent::Node(node) => {
            uses_inline_flow(node) && !matches!(node.value("width"), Some(Value::Length(_, _)))
        }
    }
}

/// The taffy children of a native container, in document order. A block
/// container groups consecutive inline-level children into anonymous-block
/// runs (see [`group_block_children`]); flex and grid containers blockify
/// every child into its own item, so each stays `Single`. `build_taffy_node`
/// and `walk` must agree on this list — it pairs taffy ids with styled nodes.
fn native_child_slots(node: &StyledNode) -> Vec<BlockChild> {
    if matches!(display_value(node), Display::Flex | Display::Grid) {
        node.children
            .iter()
            .enumerate()
            .filter(|(_, child)| !is_display_none(child) && !is_layout_whitespace_text(child))
            .map(|(i, _)| BlockChild::Single(i))
            .collect()
    } else {
        group_block_children(&node.children)
    }
}

fn build_taffy_node(tree: &mut TaffyTree<NodeBoundary>, node: &StyledNode) -> Option<NodeId> {
    if is_native_block(node) {
        let style = to_taffy_style(node);
        let mut children = Vec::new();
        for slot in native_child_slots(node) {
            let id = match slot {
                BlockChild::Single(i) => build_taffy_node(tree, &node.children[i])?,
                BlockChild::InlineRun(range) => tree
                    .new_leaf_with_context(
                        Style {
                            display: Display::Block,
                            ..Style::DEFAULT
                        },
                        NodeBoundary::new(BoundaryContent::InlineRun {
                            children: node.children[range].to_vec(),
                            align: inline_align_for(node),
                        }),
                    )
                    .ok()?,
            };
            children.push(id);
        }
        tree.new_with_children(style, &children).ok()
    } else {
        let style = boundary_taffy_style(node);
        tree.new_leaf_with_context(
            style,
            NodeBoundary::new(BoundaryContent::Node(node.clone())),
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
        width: lpa_or_auto(node.value("min-width")),
        height: LengthPercentageAuto::auto(),
    };
    style.max_size = Size {
        width: lpa_or_auto(node.value("max-width")),
        height: LengthPercentageAuto::auto(),
    };
    // Item-level properties a flex / grid parent reads. They were missing,
    // so a boundary item (anything holding inline content) never grew and
    // ignored its grid placement — hidden until 0c F11 because its
    // max-content basis used to be the 1e6 probe width (compat round 3 R2).
    // A block-flow parent ignores them.
    style.flex_grow = number_value(node.value("flex-grow")).unwrap_or(0.0);
    style.flex_shrink = number_value(node.value("flex-shrink")).unwrap_or(1.0);
    style.flex_basis = dimension(node.value("flex-basis"));
    style.align_self = align_self_value(node);
    style.grid_column = grid_line_pair(node.value("grid-column"));
    style.grid_row = grid_line_pair(node.value("grid-row"));
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
    matches!(
        node.value("grid-template-areas"),
        Some(Value::TemplateAreas(_))
    )
}

fn has_percent_inset_or_padding(node: &StyledNode) -> bool {
    let percent = |key: &str| matches!(node.value(key), Some(Value::Length(_, Unit::Percent)));
    // Percent vertical padding/inset stay on the legacy path (a boundary
    // leaf): legacy resolves padding % against the width (as the spec says)
    // and, since step 3c, relative `top`/`bottom` % against the definite
    // containing-block height (`auto` when indefinite) — moving these nodes
    // to taffy would change which layout path places them, not the answer.
    percent("padding-top") || percent("padding-bottom") || percent("top") || percent("bottom")
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
        for (slot, child_id) in native_child_slots(node).into_iter().zip(child_ids) {
            match slot {
                BlockChild::Single(i) => {
                    child_layouts.push(walk(tree, child_id, &node.children[i], next_origin));
                }
                BlockChild::InlineRun(_) => {
                    let run = boundary_box(tree, child_id, next_origin);
                    child_layouts.extend(run.children);
                }
            }
        }

        LayoutBox {
            box_type: container_box_type(node),
            dimensions,
            children: child_layouts,
        }
    } else {
        boundary_box(tree, id, parent_border_origin)
    }
}

/// A boundary leaf's cached LayoutBox, moved into place. The cache sits at
/// outer (0, 0); shift its outer-left to (abs_border_x - margin.left),
/// outer-top similarly, so the border-box lands where taffy put the leaf.
fn boundary_box(
    tree: &TaffyTree<NodeBoundary>,
    id: NodeId,
    parent_border_origin: (f32, f32),
) -> LayoutBox {
    let layout = tree.layout(id).expect("layout was just computed");
    let abs_outer_x = parent_border_origin.0 + layout.location.x - layout.margin.left;
    let abs_outer_y = parent_border_origin.1 + layout.location.y - layout.margin.top;
    let ctx = tree
        .get_node_context(id)
        .expect("boundary leaves carry a NodeBoundary context");
    // The cache holds the legacy layout from the *last* measure call, which
    // is not necessarily at taffy's final size: taffy may finish from its
    // own cache after an intrinsic-size probe (a flex item that grew from
    // its max-content basis kept its text width — compat round 3 R2). Lay
    // out again at the final border-box width (+ the leaf's own margins,
    // as in `measure_boundary`) when the cache is missing or doesn't match.
    let final_width = layout.size.width;
    let cached = ctx.cached.borrow_mut().take();
    let mut cached = match cached {
        Some(lb) if (border_box_width(&lb) - final_width).abs() < 0.01 => lb,
        _ => ctx.layout(final_width + ctx.horizontal_margins(final_width)),
    };
    shift_layout_subtree(&mut cached, abs_outer_x, abs_outer_y);
    cached
}

fn border_box_width(lb: &LayoutBox) -> f32 {
    let d = &lb.dimensions;
    d.content.width + d.padding.left + d.padding.right + d.border.left + d.border.right
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
        let bridged =
            layout_via_taffy(&styled, 800.0, None).expect("leaf block must be supported in 4.3b");

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
        let bridged = layout_via_taffy(&styled, 800.0, None)
            .expect("element root always routes through taffy after 4.3.1");
        assert_eq!(legacy.dimensions.content, bridged.dimensions.content);
        assert_eq!(legacy.children.len(), bridged.children.len());
        assert_eq!(
            legacy.children[0].dimensions.content,
            bridged.children[0].dimensions.content
        );
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
        let bridged = layout_via_taffy(&styled, 800.0, None)
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
        let bridged = layout_via_taffy(&styled, 800.0, None)
            .expect("relative + px inset must be supported in 4.3e");
        assert_eq!(
            legacy.children[0].dimensions.content,
            bridged.children[0].dimensions.content
        );
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
        let bridged = layout_via_taffy(&styled, 800.0, None)
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
