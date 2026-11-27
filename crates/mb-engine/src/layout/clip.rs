// Effective overflow clip per box (compat round 3 step 3, docs/compat-round3.md
// "3단계 — overflow 클리핑").
//
// One table, computed once after layout, that paint, hit testing, the agent's
// click target and the corpus probe all read — so the four can't disagree
// about what is clipped. Keyed by `NodeId`: an anonymous box takes the clip
// of its nearest node ancestor (the caller knows it from its walk).
//
// Rules (the design doc's, in one walk):
// - A box clips its descendants to its padding box when either `overflow`
//   axis isn't `visible` (`overflow-x: hidden` alone clips both axes — the
//   other computes to `auto`; only `clip` + `visible` clips one axis). The
//   root `<html>` and `<body>` don't clip (their overflow goes to the
//   viewport, which has no scroll model here), nor `display: inline` boxes.
// - `clip: rect()` (absolute/fixed only) and `clip-path: inset()` clip the
//   box itself and its descendants; `auto` edges are the border box's.
// - Two clips travel down: `flow` for in-flow descendants and `abs` for
//   `position: absolute` ones — the `flow` clip in force where the nearest
//   positioned ancestor was entered. A fixed box is never clipped by an
//   ancestor (its containing block is the viewport). An absolute/fixed box
//   starts its descendants' `flow` from its own clip, so an outer static
//   overflow box doesn't cut a dropdown's text while leaving its background.
// - "Positioned" = relative/absolute/fixed, exactly the boxes the layout
//   (`absolute::reposition_absolutes`) uses as containing blocks and the
//   painter lifts into z-layers. Chrome also makes `transform` and `sticky`
//   boxes containing blocks; the layout doesn't, so neither does this walk
//   (the clip must follow where the box was actually placed).
// - Coordinates: page space *after* axis-aligned transforms — what the
//   painter emits and the hit test compares against. A clip box's padding
//   box goes through the matrix in force at that box (its own transform
//   included), so a carousel track translated inside an `overflow: hidden`
//   viewport moves under a clip that stays put. A box under a rotation
//   doesn't add a clip (its clip would be a rotated quad): showing too much
//   is less bad than hiding what should show. Clips from above the rotation
//   still apply (in page space).

use std::collections::HashMap;

use crate::{
    css::{ClipEdge, ClipFunction, Unit, Value},
    dom::{NodeId, NodeType},
    render::{Affine, transform_for},
    style::StyledNode,
};

use super::{LayoutBox, Rect};

/// Each clipped box's effective clip rectangle, in page (layout) coordinates,
/// already intersected with every clipping ancestor's. A node that isn't in
/// the table paints unclipped. A zero-area entry (`clip: rect(0 0 0 0)`,
/// `width: 1px; height: 0; overflow: hidden`) hides its whole box.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClipTable {
    clips: HashMap<NodeId, Rect>,
}

impl ClipTable {
    /// The clip the boxes of `node` paint under; `None` = unclipped.
    pub fn clip_of(&self, node: NodeId) -> Option<Rect> {
        self.clips.get(&node).copied()
    }

    /// What is left of `rect` (a box of `node`) after its clip: `rect` itself
    /// when unclipped, `None` when nothing of it is visible.
    pub fn visible_rect(&self, node: NodeId, rect: Rect) -> Option<Rect> {
        match self.clip_of(node) {
            None => Some(rect),
            Some(clip) => intersect(rect, clip),
        }
    }

    /// Number of clipped nodes.
    pub fn len(&self) -> usize {
        self.clips.len()
    }

    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }
}

/// The effective clip of every box under `root`. Iterative (layout trees
/// are as deep as the DOM); one visit per box.
pub fn compute_clips(root: &LayoutBox) -> ClipTable {
    let mut table = ClipTable::default();
    let mut stack = vec![(root, Context::default())];
    while let Some((layout_box, context)) = stack.pop() {
        let inner = visit(layout_box, &context, &mut table);
        for child in layout_box.children.iter().rev() {
            stack.push((child, inner));
        }
    }
    table
}

/// What a box inherits from its ancestors.
#[derive(Clone, Copy)]
struct Context {
    /// Clip for in-flow (static/relative/sticky) boxes.
    flow: Option<Rect>,
    /// Clip for `position: absolute` boxes.
    abs: Option<Rect>,
    /// The paint walk's inherited matrix (ancestors' transforms).
    matrix: Affine,
}

impl Default for Context {
    fn default() -> Self {
        Self {
            flow: None,
            abs: None,
            matrix: Affine::IDENTITY,
        }
    }
}

/// Record `layout_box`'s own clip and return what its children inherit.
fn visit(layout_box: &LayoutBox, context: &Context, table: &mut ClipTable) -> Context {
    let matrix = context.matrix.compose(transform_for(layout_box));
    let Some(node) = layout_box.styled_node() else {
        // Anonymous boxes are in-flow and clip nothing.
        return Context { matrix, ..*context };
    };
    let position = keyword(node, "position");
    let mut own = match position {
        Some("absolute") => context.abs,
        Some("fixed") => None,
        _ => context.flow,
    };
    let axis_aligned = matrix.is_axis_aligned();
    let border = layout_box.dimensions.border_box();
    if axis_aligned
        && matches!(position, Some("absolute" | "fixed"))
        && let Some(rect) = clip_rect(node, border)
    {
        own = Some(meet(own, map_rect(matrix, rect)));
    }
    if axis_aligned && let Some(rect) = clip_path_inset(node, border) {
        own = Some(meet(own, map_rect(matrix, rect)));
    }
    if let Some(rect) = own {
        table.clips.insert(node.node_id, rect);
    }

    // Descendants: an out-of-flow box restarts the flow clip from its own.
    let mut inner = Context {
        flow: own,
        matrix,
        ..*context
    };
    if axis_aligned && let Some(rect) = overflow_clip(layout_box, node) {
        inner.flow = Some(meet(inner.flow, map_rect(matrix, rect)));
    }
    if matches!(position, Some("relative" | "absolute" | "fixed")) {
        inner.abs = inner.flow;
    }
    inner
}

/// `a ∩ b` where `None` = unclipped; an empty intersection is a zero-size
/// clip at `b` (hides everything).
fn meet(a: Option<Rect>, b: Rect) -> Rect {
    match a {
        None => b,
        Some(a) => intersect(a, b).unwrap_or(Rect {
            x: b.x,
            y: b.y,
            width: 0.0,
            height: 0.0,
        }),
    }
}

/// `rect` under an axis-aligned `matrix` (negative scale flips normalised).
fn map_rect(matrix: Affine, rect: Rect) -> Rect {
    if matrix.is_identity() {
        return rect;
    }
    let (x0, y0) = matrix.apply_point(rect.x, rect.y);
    let (x1, y1) = matrix.apply_point(rect.x + rect.width, rect.y + rect.height);
    Rect {
        x: x0.min(x1),
        y: y0.min(y1),
        width: (x1 - x0).abs(),
        height: (y1 - y0).abs(),
    }
}

/// Half-extent of an unclipped axis (a one-axis `overflow: clip`): far past
/// any layout coordinate (±`MAX_LAYOUT_COORDINATE`), small enough to keep
/// f32 sums exact to the pixel.
const UNBOUNDED: f32 = 1.0e8;

/// The padding-box clip `overflow` puts on `node`'s descendants, if any.
fn overflow_clip(layout_box: &LayoutBox, node: &StyledNode) -> Option<Rect> {
    if let NodeType::Element(element) = &node.node_type
        && (element.tag_name.eq_ignore_ascii_case("html")
            || element.tag_name.eq_ignore_ascii_case("body"))
    {
        return None;
    }
    if is_inline_non_replaced(node) {
        return None;
    }
    let axis = |name: &str| keyword(node, name).or_else(|| keyword(node, "overflow"));
    let (x, y) = (axis("overflow-x"), axis("overflow-y"));
    let clips =
        |k: Option<&str>| matches!(k, Some("hidden" | "clip" | "scroll" | "auto" | "overlay"));
    let (clip_x, clip_y) = match (clips(x), clips(y)) {
        (false, false) => return None,
        // `visible` next to anything but `clip` computes to `auto`.
        (true, false) => (true, x != Some("clip")),
        (false, true) => (y != Some("clip"), true),
        (true, true) => (true, true),
    };
    let padding = layout_box.dimensions.padding_box();
    Some(Rect {
        x: if clip_x { padding.x } else { -UNBOUNDED },
        y: if clip_y { padding.y } else { -UNBOUNDED },
        width: if clip_x {
            padding.width
        } else {
            2.0 * UNBOUNDED
        },
        height: if clip_y {
            padding.height
        } else {
            2.0 * UNBOUNDED
        },
    })
}

/// `clip: rect(top, right, bottom, left)` — offsets from the border box's
/// top-left; `auto` is that side's own edge. An edge in a unit the style
/// pass didn't resolve (only px reach here in practice) drops the clip —
/// not clipping is the safe side.
fn clip_rect(node: &StyledNode, border: Rect) -> Option<Rect> {
    let Some(Value::ClipBox(clip)) = node.value("clip") else {
        return None;
    };
    if clip.function != ClipFunction::Rect {
        return None;
    }
    let edge = |edge: ClipEdge, auto: f32| match edge {
        ClipEdge::Auto => Some(auto),
        ClipEdge::Length(v, Unit::Px) => Some(v),
        ClipEdge::Length(..) => None,
    };
    let [top, right, bottom, left] = clip.edges;
    let top = edge(top, 0.0)?;
    let right = edge(right, border.width)?;
    let bottom = edge(bottom, border.height)?;
    let left = edge(left, 0.0)?;
    Some(Rect {
        x: border.x + left,
        y: border.y + top,
        width: (right - left).max(0.0),
        height: (bottom - top).max(0.0),
    })
}

/// `clip-path: inset(top right bottom left)` on the border box (`%` of its
/// height for top/bottom, width for left/right). Other shapes are out of
/// scope (no clip).
fn clip_path_inset(node: &StyledNode, border: Rect) -> Option<Rect> {
    let Some(Value::ClipBox(clip)) = node.value("clip-path") else {
        return None;
    };
    if clip.function != ClipFunction::Inset {
        return None;
    }
    let inset = |edge: ClipEdge, basis: f32| match edge {
        ClipEdge::Auto => Some(0.0),
        ClipEdge::Length(v, Unit::Px) => Some(v),
        ClipEdge::Length(v, Unit::Percent) => Some(v / 100.0 * basis),
        ClipEdge::Length(..) => None,
    };
    let [top, right, bottom, left] = clip.edges;
    let top = inset(top, border.height)?;
    let right = inset(right, border.width)?;
    let bottom = inset(bottom, border.height)?;
    let left = inset(left, border.width)?;
    Some(Rect {
        x: border.x + left,
        y: border.y + top,
        width: (border.width - left - right).max(0.0),
        height: (border.height - top - bottom).max(0.0),
    })
}

/// `overflow` doesn't apply to inline boxes. Inline-level here is the
/// layout's own test (`display` or, unset, the phrasing-tag list), minus
/// what it lays out as a block anyway: `inline-block`, replaced/form
/// controls, and out-of-flow or floated boxes (blockified).
fn is_inline_non_replaced(node: &StyledNode) -> bool {
    if !super::inline::is_inline_node(node)
        || keyword(node, "display") == Some("inline-block")
        || matches!(keyword(node, "position"), Some("absolute" | "fixed"))
        || matches!(keyword(node, "float"), Some("left" | "right"))
    {
        return false;
    }
    match &node.node_type {
        NodeType::Element(e) => !matches!(
            e.tag_name.as_str(),
            "img" | "svg" | "input" | "select" | "textarea" | "button"
        ),
        NodeType::Text(_) => true,
    }
}

fn keyword<'a>(node: &'a StyledNode, name: &str) -> Option<&'a str> {
    match node.value(name) {
        Some(Value::Keyword(k)) => Some(k.as_str()),
        _ => None,
    }
}

/// `a ∩ b`, or `None` when they share no area (touching edges included).
pub fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let left = a.x.max(b.x);
    let top = a.y.max(b.y);
    let right = (a.x + a.width).min(b.x + b.width);
    let bottom = (a.y + a.height).min(b.y + b.height);
    (right > left && bottom > top).then_some(Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{css, html, style};

    /// Lay out `body` (inside `<html><body>`) with `css` at 400×300 and
    /// return the clip table plus the layout tree.
    fn clips(css_source: &str, body: &str) -> (ClipTable, LayoutBox) {
        let source = format!("<html><head></head><body>{body}</body></html>");
        let document = html::parse(&source).unwrap();
        let root = *document.roots().last().unwrap();
        let sheet = css::parse(css_source).unwrap();
        let styled = style::style_tree(&document, root, &[sheet]);
        let layout = crate::layout::layout_tree_with_viewport(&styled, 400.0, Some(300.0));
        (compute_clips(&layout), layout)
    }

    /// The first box of the element with `id`.
    fn find<'a>(layout: &'a LayoutBox, id: &str) -> &'a LayoutBox {
        fn walk<'a>(b: &'a LayoutBox, id: &str) -> Option<&'a LayoutBox> {
            if let Some(node) = b.styled_node()
                && let NodeType::Element(e) = &node.node_type
                && e.attributes.get("id").map(String::as_str) == Some(id)
            {
                return Some(b);
            }
            b.children.iter().find_map(|c| walk(c, id))
        }
        walk(layout, id).unwrap_or_else(|| panic!("no box #{id}"))
    }

    fn clip_of(table: &ClipTable, layout: &LayoutBox, id: &str) -> Option<Rect> {
        table.clip_of(find(layout, id).styled_node().unwrap().node_id)
    }

    /// The clip of the first text box under `#id`.
    fn text_clip_of(table: &ClipTable, layout: &LayoutBox, id: &str) -> Option<Rect> {
        fn text(b: &LayoutBox) -> Option<&StyledNode> {
            match b.styled_node() {
                Some(n) if matches!(n.node_type, NodeType::Text(_)) => Some(n),
                _ => b.children.iter().find_map(text),
            }
        }
        table.clip_of(text(find(layout, id)).expect("a text box").node_id)
    }

    const BOX: &str = "#c { width: 100px; height: 50px; overflow: hidden; }";

    #[test]
    fn overflow_hidden_clips_descendants_to_padding_box_not_itself() {
        let (table, layout) = clips(
            "body { margin: 0 } #c { width: 100px; height: 50px; padding: 5px; overflow: hidden; border: 2px solid; }",
            "<div id=c><div id=k style='height:200px'>x</div></div>",
        );
        assert_eq!(
            clip_of(&table, &layout, "c"),
            None,
            "a box doesn't clip itself"
        );
        assert_eq!(
            clip_of(&table, &layout, "k"),
            Some(Rect {
                x: 2.0,
                y: 2.0,
                width: 110.0,
                height: 60.0
            })
        );
    }

    #[test]
    fn static_overflow_parent_does_not_clip_absolute_child() {
        // The absolute child's containing block is outside the overflow box.
        let (table, layout) = clips(
            BOX,
            "<div id=c><span id=a style='position:absolute;top:0;left:0'>x</span></div>",
        );
        assert_eq!(clip_of(&table, &layout, "a"), None);
        assert_eq!(text_clip_of(&table, &layout, "a"), None);
    }

    #[test]
    fn relative_wrapper_lets_overflow_clip_absolute_grandchild() {
        // A tree-wrapping painter would get this wrong: the absolute span is
        // painted in a z-layer, but its containing block (the relative div)
        // is inside the overflow box.
        let (table, layout) = clips(
            BOX,
            "<div id=c><div style='position:relative'><span id=a style='position:absolute'>x</span></div></div>",
        );
        let c = find(&layout, "c").dimensions.padding_box();
        assert_eq!(clip_of(&table, &layout, "a"), Some(c));
        assert_eq!(text_clip_of(&table, &layout, "a"), Some(c));
    }

    #[test]
    fn positioned_overflow_box_clips_its_absolute_children() {
        let (table, layout) = clips(
            "#c { position: relative; width: 100px; height: 50px; overflow: hidden; }",
            "<div id=c><span id=a style='position:absolute;left:150px'>x</span></div>",
        );
        let c = find(&layout, "c").dimensions.padding_box();
        assert_eq!(clip_of(&table, &layout, "a"), Some(c));
    }

    #[test]
    fn fixed_child_escapes_overflow_clip() {
        let (table, layout) = clips(
            "#c { position: relative; width: 100px; height: 50px; overflow: hidden; }",
            "<div id=c><div id=f style='position:fixed;top:0'>x</div></div>",
        );
        assert_eq!(clip_of(&table, &layout, "f"), None);
        assert_eq!(text_clip_of(&table, &layout, "f"), None);
    }

    #[test]
    fn transformed_box_is_not_a_containing_block_here() {
        // Chrome: a transform makes the box the containing block of its
        // absolute children (so its overflow would clip them). Our layout
        // doesn't place them that way, so the clip doesn't follow either.
        let (table, layout) = clips(
            "#c { width: 100px; height: 50px; overflow: hidden; transform: translateX(1px); }",
            "<div id=c><div id=a style='position:absolute;top:0'>x</div></div>",
        );
        assert_eq!(clip_of(&table, &layout, "a"), None);
    }

    #[test]
    fn absolute_box_resets_flow_clip_for_its_text() {
        // A dropdown: absolute inside a static overflow box whose containing
        // block is outside it. Its background and its text must agree —
        // both unclipped (before the reset, the text inherited the static
        // box's flow clip and vanished while the background showed).
        let (table, layout) = clips(
            "#c { width: 100px; height: 10px; overflow: hidden; } #d { position: absolute; top: 40px; background: red; }",
            "<div style='position:relative'><div id=c><div id=d>menu item</div></div></div>",
        );
        assert_eq!(clip_of(&table, &layout, "d"), None);
        assert_eq!(text_clip_of(&table, &layout, "d"), None);
    }

    #[test]
    fn nested_clips_intersect() {
        let (table, layout) = clips(
            "body { margin: 0 } #o { width: 100px; height: 100px; overflow: hidden; } #i { margin-left: 50px; width: 100px; height: 30px; overflow: hidden; }",
            "<div id=o><div id=i><div id=k>x</div></div></div>",
        );
        assert_eq!(
            clip_of(&table, &layout, "k"),
            Some(Rect {
                x: 50.0,
                y: 0.0,
                width: 50.0,
                height: 30.0
            })
        );
    }

    #[test]
    fn overflow_x_hidden_clips_both_axes() {
        // `overflow-y: visible` next to `hidden` computes to `auto`.
        let (table, layout) = clips(
            "#c { width: 100px; height: 50px; overflow-x: hidden; }",
            "<div id=c><div id=k>x</div></div>",
        );
        assert_eq!(
            clip_of(&table, &layout, "k"),
            Some(find(&layout, "c").dimensions.padding_box())
        );
    }

    #[test]
    fn overflow_x_clip_with_visible_y_clips_one_axis() {
        let (table, layout) = clips(
            "#c { width: 100px; height: 50px; overflow-x: clip; overflow-y: visible; }",
            "<div id=c><div id=k>x</div></div>",
        );
        let clip = clip_of(&table, &layout, "k").unwrap();
        let c = find(&layout, "c").dimensions.padding_box();
        assert_eq!((clip.x, clip.width), (c.x, c.width));
        assert!(clip.height > 1.0e6, "y is unclipped: {clip:?}");
    }

    #[test]
    fn inline_overflow_hidden_does_not_clip() {
        let (table, layout) = clips(
            "#c { overflow: hidden; width: 1px; height: 1px; }",
            "<p><span id=c>long inline text</span></p>",
        );
        assert_eq!(text_clip_of(&table, &layout, "c"), None);
    }

    #[test]
    fn body_overflow_hidden_does_not_clip_page() {
        let (table, layout) = clips(
            "html, body { height: 100%; overflow: hidden; }",
            "<div id=k style='margin-top:1000px'>far below</div>",
        );
        assert_eq!(clip_of(&table, &layout, "k"), None);
        assert!(table.is_empty());
    }

    #[test]
    fn sr_only_clip_rect_is_zero_area_for_the_box_and_its_text() {
        let (table, layout) = clips(
            ".sr { position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0); white-space: nowrap; }",
            "<a id=s class=sr href='#m'>Skip to content</a>",
        );
        for clip in [
            clip_of(&table, &layout, "s").unwrap(),
            text_clip_of(&table, &layout, "s").unwrap(),
        ] {
            assert_eq!(clip.width * clip.height, 0.0, "{clip:?}");
        }
    }

    #[test]
    fn clip_rect_on_static_box_is_ignored() {
        let (table, layout) = clips(
            "#c { clip: rect(0 0 0 0); }",
            "<div id=c>still visible</div>",
        );
        assert_eq!(text_clip_of(&table, &layout, "c"), None);
    }

    #[test]
    fn clip_rect_auto_edges_are_the_border_box() {
        let (table, layout) = clips(
            "#c { position: absolute; top: 0; left: 0; width: 100px; height: 50px; clip: rect(10px, auto, auto, 20px); }",
            "<div id=c>x</div>",
        );
        let b = find(&layout, "c").dimensions.border_box();
        assert_eq!(
            clip_of(&table, &layout, "c"),
            Some(Rect {
                x: b.x + 20.0,
                y: b.y + 10.0,
                width: b.width - 20.0,
                height: b.height - 10.0
            })
        );
    }

    #[test]
    fn clip_path_inset_50_hides_subtree() {
        let (table, layout) = clips(
            "#c { width: 100px; height: 50px; clip-path: inset(50%); }",
            "<div id=c><div id=k>x</div></div>",
        );
        for clip in [clip_of(&table, &layout, "c"), clip_of(&table, &layout, "k")] {
            let clip = clip.unwrap();
            assert_eq!(clip.width * clip.height, 0.0, "{clip:?}");
        }
    }

    #[test]
    fn translated_carousel_track_keeps_the_viewport_clip() {
        // The track moves; the window's clip stays where the window is.
        let (table, layout) = clips(
            "body { margin: 0 } #w { width: 100px; height: 40px; overflow: hidden; } #t { width: 300px; transform: translateX(-100px); }",
            "<div id=w><div id=t><div id=k>slide</div></div></div>",
        );
        assert_eq!(
            clip_of(&table, &layout, "k"),
            Some(Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 40.0
            })
        );
    }

    #[test]
    fn scaled_overflow_box_clips_in_page_space() {
        let (table, layout) = clips(
            "body { margin: 0 } #c { width: 100px; height: 40px; overflow: hidden; transform: scale(2); }",
            "<div id=c><div id=k>x</div></div>",
        );
        // Scaled about the centre (50, 20): 200×80 at (-50, -20).
        assert_eq!(
            clip_of(&table, &layout, "k"),
            Some(Rect {
                x: -50.0,
                y: -20.0,
                width: 200.0,
                height: 80.0
            })
        );
    }

    #[test]
    fn rotated_ancestor_skips_clip() {
        // A clip under a rotation would be a rotated quad: not applied (the
        // content overflows rather than vanishing).
        let (table, layout) = clips(
            "#r { transform: rotate(10deg); } #c { width: 100px; height: 40px; overflow: hidden; }",
            "<div id=r><div id=c><div id=k>x</div></div></div>",
        );
        assert_eq!(clip_of(&table, &layout, "k"), None);
    }

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn intersect_overlapping_disjoint_and_touching_rects() {
        assert_eq!(
            intersect(rect(0.0, 0.0, 10.0, 10.0), rect(5.0, 5.0, 10.0, 10.0)),
            Some(rect(5.0, 5.0, 5.0, 5.0))
        );
        assert_eq!(
            intersect(rect(0.0, 0.0, 10.0, 10.0), rect(20.0, 0.0, 5.0, 5.0)),
            None
        );
        // Sharing an edge is no area.
        assert_eq!(
            intersect(rect(0.0, 0.0, 10.0, 10.0), rect(10.0, 0.0, 5.0, 5.0)),
            None
        );
        // A zero-area clip hides everything.
        assert_eq!(
            intersect(rect(0.0, 0.0, 10.0, 10.0), rect(2.0, 2.0, 0.0, 0.0)),
            None
        );
    }
}
