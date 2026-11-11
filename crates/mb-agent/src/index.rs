// One pass over the DOM and the layout tree, building the lookups the role /
// name / text code needs: document order, `id` attribute → node,
// `<label for>` → label, and node → laid-out rect.
//
// BTreeMap rather than HashMap throughout: these maps are only lookups today,
// but keeping them ordered means nothing iteration-order-dependent can sneak
// into the observation later (determinism rule, AGENTS.md).

use std::collections::BTreeMap;

use mb_runtime::dom::{Document, ElementData, NodeId, NodeType};
use mb_runtime::layout::{BoxType, LayoutBox, Rect};

use crate::text::{collapse_whitespace, push_text_piece};

pub(crate) struct DomIndex<'a> {
    pub(crate) document: &'a Document,
    /// Every live node, in document (pre-order DFS) order.
    pub(crate) order: Vec<NodeId>,
    ids: BTreeMap<String, NodeId>,
    labels_for: BTreeMap<String, NodeId>,
    rects: BTreeMap<NodeId, Rect>,
}

impl<'a> DomIndex<'a> {
    pub(crate) fn build(document: &'a Document, layout_root: &LayoutBox) -> Self {
        let mut index = Self {
            document,
            order: Vec::new(),
            ids: BTreeMap::new(),
            labels_for: BTreeMap::new(),
            rects: BTreeMap::new(),
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
    /// a `<label>`, or `<script>`).
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
            NodeType::Text(text) => push_text_piece(out, text),
            NodeType::Element(_) => {
                if skip(id) {
                    return;
                }
                for &child in &node.children {
                    self.push_text(child, skip, out);
                }
            }
        }
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
