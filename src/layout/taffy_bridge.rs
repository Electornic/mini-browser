// 4.3a: bridge is not yet wired into dispatch — sub-phases 4.3b/c/d/e will
// pull `to_taffy_style` and the helpers below into block/flex/grid/table.
#![allow(dead_code)]

// CSS → taffy::Style converter. Phase 4.3a wires this read-only — sibling
// algorithm modules will switch their dispatch to `taffy::compute_layout` in
// 4.3b/c/d/e and start consuming this output. Em/rem are already resolved to
// px at style time (see `style.rs`), so we only see `Px` and `Percent` here.
// Calc()/min()/max() are not in the AST yet and fall through to defaults.
//
// Anything we don't understand yet keeps taffy's `Style::DEFAULT` value, which
// matches CSS initial values for the relevant property.

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
    Dimensions, EdgeSizes, LayoutBox, Rect, container_box_type, is_display_none,
    is_layout_whitespace_text, is_out_of_flow,
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

    style.size = Size {
        width: dimension(node.value("width")),
        height: dimension(node.value("height")),
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
        left: lpa(node.value("left")),
        right: lpa(node.value("right")),
        top: lpa(node.value("top")),
        bottom: lpa(node.value("bottom")),
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

fn lpa(value: Option<&Value>) -> LengthPercentageAuto {
    match value {
        Some(Value::Length(v, Unit::Px)) => LengthPercentageAuto::length(*v),
        Some(Value::Length(v, Unit::Percent)) => LengthPercentageAuto::percent(*v / 100.0),
        Some(Value::Keyword(kw)) if kw == "auto" => LengthPercentageAuto::auto(),
        _ => LengthPercentageAuto::auto(),
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
    TaffyRect {
        left: lpa(node.value(&format!("{prefix}-left"))),
        right: lpa(node.value(&format!("{prefix}-right"))),
        top: lpa(node.value(&format!("{prefix}-top"))),
        bottom: lpa(node.value(&format!("{prefix}-bottom"))),
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
    TaffyRect {
        left: lp(node.value("border-left-width")),
        right: lp(node.value("border-right-width")),
        top: lp(node.value("border-top-width")),
        bottom: lp(node.value("border-bottom-width")),
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
    tree.compute_layout(
        root_id,
        Size {
            width: AvailableSpace::Definite(viewport_width),
            height: AvailableSpace::MaxContent,
        },
    )
    .ok()?;
    let root_layout = tree.layout(root_id).ok()?;
    // Root's outer (margin-box) top-left lives at (0, 0) in our coord system,
    // which means the root's BORDER box sits at (margin.left, margin.top).
    // Children's `location` is relative to the parent's border box, so this
    // origin propagates naturally during walk-back.
    let root_origin = (root_layout.margin.left, root_layout.margin.top);
    Some(walk_back(&tree, root_id, node, root_origin))
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
    {
        return false;
    }
    // 4.3b accepts only leaf blocks — every child must be display:none or
    // pure-whitespace text (both are skipped during taffy build, so the
    // resulting taffy node has zero children). 4.3c will lift this to allow
    // nested block subtrees with text leaves.
    node.children
        .iter()
        .all(|c| is_display_none(c) || is_layout_whitespace_text(c))
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
    fn taffy_rejects_unsupported_subtree_in_4_3b() {
        // Block element with element children — not handled in 4.3b yet.
        let styled = styled_root(
            r#"<div id="root"><p>One</p></div>"#,
            r#"#root { width: 300px; } p { font-size: 16px; }"#,
        );
        assert!(layout_via_taffy(&styled, 800.0).is_none());
    }
}
