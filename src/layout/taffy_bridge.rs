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
    Dimension, LengthPercentage, LengthPercentageAuto, TaffyGridLine, TaffyGridSpan,
};
use taffy::style::{
    AlignContent, AlignItems, AlignSelf, BoxSizing, Display, FlexDirection, FlexWrap,
    GridAutoFlow, GridPlacement as TaffyGridPlacement, GridTemplateComponent, JustifyContent,
    MaxTrackSizingFunction, MinTrackSizingFunction, Overflow, Position, Style, TrackSizingFunction,
};
use taffy::geometry::{Line, MinMax, Rect, Size};

use crate::css::{GridLine, TrackSize, Unit, Value};
use crate::style::StyledNode;

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
    style.inset = Rect {
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

fn edge_lpa(node: &StyledNode, prefix: &str) -> Rect<LengthPercentageAuto> {
    Rect {
        left: lpa(node.value(&format!("{prefix}-left"))),
        right: lpa(node.value(&format!("{prefix}-right"))),
        top: lpa(node.value(&format!("{prefix}-top"))),
        bottom: lpa(node.value(&format!("{prefix}-bottom"))),
    }
}

fn edge_lp(node: &StyledNode, prefix: &str) -> Rect<LengthPercentage> {
    Rect {
        left: lp(node.value(&format!("{prefix}-left"))),
        right: lp(node.value(&format!("{prefix}-right"))),
        top: lp(node.value(&format!("{prefix}-top"))),
        bottom: lp(node.value(&format!("{prefix}-bottom"))),
    }
}

fn border_widths(node: &StyledNode) -> Rect<LengthPercentage> {
    Rect {
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
