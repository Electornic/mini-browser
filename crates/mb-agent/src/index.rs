// One pass over the DOM and the layout tree, building the lookups the role /
// name / text code needs: document order, `id` attribute → node,
// `<label for>` → label, and node → laid-out rect.
//
// BTreeMap rather than HashMap throughout: these maps are only lookups today,
// but keeping them ordered means nothing iteration-order-dependent can sneak
// into the observation later (determinism rule, AGENTS.md).

use std::collections::{BTreeMap, BTreeSet};

use mb_runtime::dom::{Document, ElementData, NodeId, NodeType};
use mb_runtime::layout::{BoxType, ClipTable, LayoutBox, Rect};
use mb_runtime::render::{self, Affine};

use crate::text::{collapse_whitespace, push_text_piece};

pub(crate) struct DomIndex<'a> {
    pub(crate) document: &'a Document,
    /// Every live node, in document (pre-order DFS) order.
    pub(crate) order: Vec<NodeId>,
    ids: BTreeMap<String, NodeId>,
    labels_for: BTreeMap<String, NodeId>,
    rects: BTreeMap<NodeId, Rect>,
    /// Nodes whose computed `visibility` hides them (compat round 4 1a).
    invisible: BTreeSet<NodeId>,
    /// Nodes inside an `inert` subtree (computed — N4).
    inert: BTreeSet<NodeId>,
    click_listeners: BTreeSet<NodeId>,
}

impl<'a> DomIndex<'a> {
    pub(crate) fn build(
        document: &'a Document,
        layout_root: &LayoutBox,
        click_listeners: &[NodeId],
    ) -> Self {
        let mut index = Self {
            document,
            order: Vec::new(),
            ids: BTreeMap::new(),
            labels_for: BTreeMap::new(),
            rects: BTreeMap::new(),
            invisible: BTreeSet::new(),
            inert: BTreeSet::new(),
            click_listeners: click_listeners.iter().copied().collect(),
        };
        for &root in document.roots() {
            index.visit(root);
        }
        index.collect_rects(layout_root);
        index
    }

    fn visit(&mut self, id: NodeId) {
        let Some(node) = self.document.get(id) else {
            return;
        };
        self.order.push(id);
        if let NodeType::Element(element) = &node.node_type {
            if let Some(attr_id) = element.attributes.get("id") {
                self.ids.entry(attr_id.clone()).or_insert(id);
            }
            if element.tag_name == "label"
                && let Some(target) = element.attributes.get("for")
            {
                self.labels_for.entry(target.clone()).or_insert(id);
            }
        }
        for &child in &node.children {
            self.visit(child);
        }
    }

    // Layout coordinates are already document-absolute (scroll is applied
    // only at paint / hit-test time), so the border box is stored as-is.
    // A node that owns several boxes gets the union of them.
    fn collect_rects(&mut self, layout_box: &LayoutBox) {
        let styled = match &layout_box.box_type {
            BoxType::BlockNode(node)
            | BoxType::FlexNode(node)
            | BoxType::GridNode(node)
            | BoxType::TableNode(node) => Some(node),
            BoxType::AnonymousBlock => None,
        };
        if let Some(node) = styled {
            let d = &layout_box.dimensions;
            let rect = Rect {
                x: d.content.x - d.padding.left - d.border.left,
                y: d.content.y - d.padding.top - d.border.top,
                width: d.border.left
                    + d.padding.left
                    + d.content.width
                    + d.padding.right
                    + d.border.right,
                height: d.border.top
                    + d.padding.top
                    + d.content.height
                    + d.padding.bottom
                    + d.border.bottom,
            };
            self.rects
                .entry(node.node_id)
                .and_modify(|existing| *existing = union(*existing, rect))
                .or_insert(rect);
            if node.visibility_hidden() {
                self.invisible.insert(node.node_id);
            }
            if node.inert() {
                self.inert.insert(node.node_id);
            }
        }
        for child in &layout_box.children {
            self.collect_rects(child);
        }
    }

    pub(crate) fn element(&self, id: NodeId) -> Option<&'a ElementData> {
        self.document.element_data(id)
    }

    pub(crate) fn tag(&self, id: NodeId) -> Option<&'a str> {
        self.element(id).map(|e| e.tag_name.as_str())
    }

    pub(crate) fn attr(&self, id: NodeId, name: &str) -> Option<&'a str> {
        self.element(id)?.attributes.get(name).map(String::as_str)
    }

    pub(crate) fn has_attr(&self, id: NodeId, name: &str) -> bool {
        self.attr(id, name).is_some()
    }

    pub(crate) fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.document.get(id)?.parent
    }

    pub(crate) fn children(&self, id: NodeId) -> &'a [NodeId] {
        self.document
            .get(id)
            .map(|node| node.children.as_slice())
            .unwrap_or(&[])
    }

    pub(crate) fn rect(&self, id: NodeId) -> Option<Rect> {
        self.rects.get(&id).copied()
    }

    /// Computed `visibility: hidden`/`collapse` (its box exists and keeps
    /// its space, but nothing of it shows).
    pub(crate) fn is_invisible(&self, id: NodeId) -> bool {
        self.invisible.contains(&id)
    }

    /// Inside an `inert` subtree.
    pub(crate) fn is_inert(&self, id: NodeId) -> bool {
        self.inert.contains(&id)
    }

    /// Has a `click` listener registered from JS (addEventListener / on*).
    pub(crate) fn has_click_listener(&self, id: NodeId) -> bool {
        self.click_listeners.contains(&id)
    }

    pub(crate) fn by_id_attr(&self, value: &str) -> Option<NodeId> {
        self.ids.get(value).copied()
    }

    pub(crate) fn label_for(&self, id_attr: &str) -> Option<NodeId> {
        self.labels_for.get(id_attr).copied()
    }

    /// Nearest ancestor (excluding `id` itself) with the given tag.
    pub(crate) fn ancestor_with_tag(&self, id: NodeId, tag: &str) -> Option<NodeId> {
        let mut current = self.parent(id);
        while let Some(node) = current {
            if self.tag(node) == Some(tag) {
                return Some(node);
            }
            current = self.parent(node);
        }
        None
    }

    /// Whitespace-collapsed text of every descendant text node. `skip`
    /// prunes subtrees whose text shouldn't count (e.g. the `<input>` inside
    /// a `<label>`, or `<script>`) and text nodes (`visibility: hidden`
    /// text — it inherits the value, so a `visible` child still counts).
    pub(crate) fn text_content(&self, id: NodeId, skip: &dyn Fn(NodeId) -> bool) -> String {
        let mut raw = String::new();
        self.push_text(id, skip, &mut raw);
        collapse_whitespace(&raw)
    }

    fn push_text(&self, id: NodeId, skip: &dyn Fn(NodeId) -> bool, out: &mut String) {
        let Some(node) = self.document.get(id) else {
            return;
        };
        match &node.node_type {
            NodeType::Text(text) => {
                if !skip(id) {
                    push_text_piece(out, text);
                }
            }
            NodeType::Element(element) => {
                // Never-rendered content has no text to a reader; Google
                // nests `<style>` in its buttons.
                if skip(id) || is_unrendered_text_container(&element.tag_name) {
                    return;
                }
                for &child in &node.children {
                    self.push_text(child, skip, out);
                }
            }
        }
    }
}

fn is_unrendered_text_container(tag: &str) -> bool {
    matches!(tag, "script" | "style" | "template" | "noscript")
}

/// Border-box rect of `id` in the current layout (document coordinates),
/// without building a whole index. Used right before acting on an element.
pub(crate) fn find_rect(layout_box: &LayoutBox, id: NodeId) -> Option<Rect> {
    let own = match &layout_box.box_type {
        BoxType::BlockNode(node)
        | BoxType::FlexNode(node)
        | BoxType::GridNode(node)
        | BoxType::TableNode(node)
            if node.node_id == id =>
        {
            let d = &layout_box.dimensions;
            Some(Rect {
                x: d.content.x - d.padding.left - d.border.left,
                y: d.content.y - d.padding.top - d.border.top,
                width: d.border.left
                    + d.padding.left
                    + d.content.width
                    + d.padding.right
                    + d.border.right,
                height: d.border.top
                    + d.padding.top
                    + d.content.height
                    + d.padding.bottom
                    + d.border.bottom,
            })
        }
        _ => None,
    };
    layout_box
        .children
        .iter()
        .filter_map(|child| find_rect(child, id))
        .fold(own, |acc, r| Some(acc.map_or(r, |a| union(a, r))))
}

/// Where `id` can be clicked: each of its boxes' border box mapped through
/// the paint transforms (page coordinates, the space the hit test and the
/// clip table use), cut to the box's overflow clip, unioned. `None` = no
/// box; `Some(Err(()))` = boxes exist but the clip hides all of them.
/// Under a rotation the box's bounding box stands in (the hit test maps
/// the point back exactly; the bounding-box centre is inside for the
/// convex shapes boxes are).
pub(crate) fn find_visible_rect(
    layout_box: &LayoutBox,
    id: NodeId,
    clips: &ClipTable,
) -> Option<Result<Rect, ()>> {
    let mut found = false;
    let mut visible: Option<Rect> = None;
    let mut stack = vec![(layout_box, Affine::IDENTITY)];
    while let Some((b, inherited)) = stack.pop() {
        let matrix = inherited.compose(render::transform_for(b));
        if let BoxType::BlockNode(node)
        | BoxType::FlexNode(node)
        | BoxType::GridNode(node)
        | BoxType::TableNode(node) = &b.box_type
            && node.node_id == id
        {
            found = true;
            let page = page_rect(border_box(b), matrix);
            if let Some(part) = clips.visible_rect(id, page) {
                visible = Some(visible.map_or(part, |v| union(v, part)));
            }
        }
        for child in b.children.iter().rev() {
            stack.push((child, matrix));
        }
    }
    found.then_some(visible.ok_or(()))
}

fn border_box(b: &LayoutBox) -> Rect {
    let d = &b.dimensions;
    Rect {
        x: d.content.x - d.padding.left - d.border.left,
        y: d.content.y - d.padding.top - d.border.top,
        width: d.border.left + d.padding.left + d.content.width + d.padding.right + d.border.right,
        height: d.border.top
            + d.padding.top
            + d.content.height
            + d.padding.bottom
            + d.border.bottom,
    }
}

/// The axis-aligned bounding box of `rect` under `matrix`.
fn page_rect(rect: Rect, matrix: Affine) -> Rect {
    if matrix.is_identity() {
        return rect;
    }
    let corners = [
        (rect.x, rect.y),
        (rect.x + rect.width, rect.y),
        (rect.x, rect.y + rect.height),
        (rect.x + rect.width, rect.y + rect.height),
    ]
    .map(|(x, y)| matrix.apply_point(x, y));
    let min_x = corners.iter().map(|c| c.0).fold(f32::INFINITY, f32::min);
    let min_y = corners.iter().map(|c| c.1).fold(f32::INFINITY, f32::min);
    let max_x = corners
        .iter()
        .map(|c| c.0)
        .fold(f32::NEG_INFINITY, f32::max);
    let max_y = corners
        .iter()
        .map(|c| c.1)
        .fold(f32::NEG_INFINITY, f32::max);
    Rect {
        x: min_x,
        y: min_y,
        width: max_x - min_x,
        height: max_y - min_y,
    }
}

fn union(a: Rect, b: Rect) -> Rect {
    let left = a.x.min(b.x);
    let top = a.y.min(b.y);
    let right = (a.x + a.width).max(b.x + b.width);
    let bottom = (a.y + a.height).max(b.y + b.height);
    Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    }
}
