// CSS → taffy::Style converter and the `layout_via_taffy` dispatch entry
// for Phase 4.3. Em/rem are already resolved to px at style time (see
// `style.rs`), so we only see Px and Percent here. Calc()/min()/max() are
// not in the AST yet and fall through to defaults. Anything we don't
// understand keeps taffy's `Style::DEFAULT` value, which matches CSS
// initial values for the relevant property.
//
// Phase 4.3 partial status: this bridge handles the pure-block subset
// (single elements + nested empty blocks + non-percent inset). Inline-flow,
// floats/clear, flex/grid/table containers, and percent vertical
// padding/inset all bail to the legacy block algorithm via `is_supported`
// returning None — `layout_tree_with_fonts` falls back transparently.
// Future Phase 4.3.x work would route the remaining shapes through taffy
// either natively (flex/grid via `Display::Flex`/`Display::Grid`) or via a
// `compute_layout_with_measure` callback that runs legacy layout in a
// boundary leaf.

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
    Dimensions, EdgeSizes, LayoutBox, Rect, container_box_type, intrinsic_height,
    intrinsic_width, is_display_none, is_layout_whitespace_text, is_out_of_flow,
};
use super::flex::is_flex_container;
use super::grid::is_grid_container;
use super::inline::uses_inline_flow;
use super::table::is_table_container;

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

// ---------- 4.3b: end-to-end taffy dispatch (off by default) ----------
//
// `layout_via_taffy` is the new layout entry under construction. It returns
// `None` for any subtree shape we can't yet handle natively in taffy, so a
// caller can transparently fall back to the legacy block path. Each sub-phase
// (4.3c–e) widens the `is_supported` filter until the legacy path falls out.
//
// 4.3b only handles the simplest case — a single block element with no
// element children — which is enough to verify the bridge produces the same
// dimensions as the legacy code end-to-end.

pub(super) fn layout_via_taffy(node: &StyledNode, viewport_width: f32) -> Option<LayoutBox> {
    if !is_supported(node) {
        return None;
    }
    let mut tree: TaffyTree<()> = TaffyTree::new();
    let root_id = build_taffy_node(&mut tree, node)?;
    // Wrap the actual root in a synthetic block container so taffy resolves
    // the root's `margin: auto` against the viewport (taffy's outermost node
    // sits at (0, 0) without a parent block context, so auto margins on the
    // root itself otherwise collapse to 0).
    let wrapper_style = Style {
        display: Display::Block,
        size: Size {
            width: Dimension::length(viewport_width),
            height: Dimension::auto(),
        },
        ..Style::DEFAULT
    };
    let wrapper_id = tree.new_with_children(wrapper_style, &[root_id]).ok()?;
    tree.compute_layout(
        wrapper_id,
        Size {
            width: AvailableSpace::Definite(viewport_width),
            height: AvailableSpace::MaxContent,
        },
    )
    .ok()?;
    // The wrapper sits at (0, 0); the real root lives one level inside, so
    // its border-box origin in viewport coords is `wrapper.location +
    // root.location`. The wrapper has no padding/border, so this just adds
    // the root's offset within the wrapper.
    let root_layout = tree.layout(root_id).ok()?;
    let wrapper_layout = tree.layout(wrapper_id).ok()?;
    let root_origin = (
        wrapper_layout.location.x + root_layout.location.x,
        wrapper_layout.location.y + root_layout.location.y,
    );
    // Recompose the root LayoutBox using its own Layout (NOT the wrapper)
    // and the absolute origin of its border box.
    Some(walk_back_root(&tree, root_id, node, root_origin))
}

fn walk_back_root(
    tree: &TaffyTree<()>,
    id: NodeId,
    node: &StyledNode,
    abs_border_origin: (f32, f32),
) -> LayoutBox {
    // Same as `walk_back`, except the absolute border-box origin is supplied
    // directly rather than computed from a parent's origin + this node's
    // location (the wrapper hop already accounted for that).
    let layout = tree.layout(id).expect("layout was just computed");
    let abs_border_x = abs_border_origin.0;
    let abs_border_y = abs_border_origin.1;
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
        child_layouts.push(walk_back(tree, child_id, child, next_origin));
    }

    LayoutBox {
        box_type: container_box_type(node),
        dimensions,
        children: child_layouts,
    }
}

fn is_supported(node: &StyledNode) -> bool {
    if !matches!(node.node_type, NodeType::Element(_)) {
        return false;
    }
    if is_flex_container(node)
        || is_grid_container(node)
        || is_table_container(node)
        || uses_inline_flow(node)
        || is_out_of_flow(node)
        || has_float(node)
        || has_clear(node)
        || has_percent_inset_or_padding(node)
    {
        return false;
    }
    // 4.3e: `position: relative` with non-percent inset is now safe to route
    // through taffy (it positions the box statically, then shifts by the
    // inset — same observable result as the legacy post-pass). Percent inset
    // still falls back via `has_percent_inset_or_padding` because legacy
    // resolves both axes against parent width while taffy follows the spec.
    // 4.3d allows nested block subtrees: every child must be skipped
    // (display:none / pure-whitespace) or itself a supported block. The
    // remaining unsupported shapes (inline flow, floats, position, %-resolved
    // padding/inset) all bail to the legacy path for the whole subtree —
    // 4.3e wires inline/flex/grid/table.
    node.children
        .iter()
        .all(|c| is_display_none(c) || is_layout_whitespace_text(c) || is_supported(c))
}

fn has_float(node: &StyledNode) -> bool {
    matches!(node.value("float"), Some(Value::Keyword(k)) if k == "left" || k == "right")
}

fn has_clear(node: &StyledNode) -> bool {
    matches!(
        node.value("clear"),
        Some(Value::Keyword(k)) if k == "left" || k == "right" || k == "both"
    )
}

fn has_percent_inset_or_padding(node: &StyledNode) -> bool {
    let percent = |key: &str| {
        matches!(node.value(key), Some(Value::Length(_, Unit::Percent)))
    };
    // Old layout's quirk for vertical padding/inset percentages (using
    // parent_width as the base) doesn't survive the trip through taffy on
    // top/bottom — punt to legacy when these are in play.
    percent("padding-top")
        || percent("padding-bottom")
        || percent("top")
        || percent("bottom")
}

fn build_taffy_node(tree: &mut TaffyTree<()>, node: &StyledNode) -> Option<NodeId> {
    let style = to_taffy_style(node);
    let mut children = Vec::new();
    for child in &node.children {
        if is_display_none(child) || is_layout_whitespace_text(child) {
            continue;
        }
        children.push(build_taffy_node(tree, child)?);
    }
    tree.new_with_children(style, &children).ok()
}

fn walk_back(
    tree: &TaffyTree<()>,
    id: NodeId,
    node: &StyledNode,
    parent_border_origin: (f32, f32),
) -> LayoutBox {
    let layout = tree.layout(id).expect("layout was just computed");
    let abs_border_x = parent_border_origin.0 + layout.location.x;
    let abs_border_y = parent_border_origin.1 + layout.location.y;
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

    // Recurse into children. The taffy child order matches the order we built
    // them in `build_taffy_node`, which mirrors `node.children` minus skipped
    // (display:none / whitespace) ones — keep them aligned here.
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
        child_layouts.push(walk_back(tree, child_id, child, next_origin));
    }

    LayoutBox {
        box_type: container_box_type(node),
        dimensions,
        children: child_layouts,
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
    fn taffy_rejects_inline_flow_subtree() {
        // Block element with text children — inline flow is the legacy
        // path's job until 4.3e wires a measure callback for it.
        let styled = styled_root(
            r#"<div id="root"><p>One</p></div>"#,
            r#"#root { width: 300px; } p { font-size: 16px; }"#,
        );
        assert!(layout_via_taffy(&styled, 800.0).is_none());
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
