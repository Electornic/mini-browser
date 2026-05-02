use std::collections::BTreeMap;

// Attributes live in a deterministic map so debug output and snapshot-like tests stay stable.
pub type AttrMap = BTreeMap<String, String>;

// Stable handle into a `Document` arena. NodeId is a thin u32 index — the
// `Document` owns all storage so cloning a NodeId is free and sending one to
// JS native code is safe (no lifetimes, no Rc traffic for the handle itself).
//
// Wrapped (rather than a bare integer) so the type system blocks accidental
// mixing with other indexed slot kinds (e.g. layout box indices) and so the
// debug printout names the role rather than just "u32".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

impl NodeId {
    fn index(self) -> usize {
        self.0 as usize
    }

    /// Underlying slot number. Exposed for the JS bridge, which stores the
    /// raw u32 on Element wrapper objects so cross-method calls (e.g.
    /// `parent.appendChild(child)`) can recover the receiver's NodeId from
    /// any wrapper without a parallel handle table.
    pub fn raw(self) -> u32 {
        self.0
    }

    pub fn from_raw(raw: u32) -> Self {
        Self(raw)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeType {
    Element(ElementData),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementData {
    pub tag_name: String,
    pub attributes: AttrMap,
}

// One slot in the arena. Children are NodeIds rather than nested NodeData so
// the same physical storage backs every reference to a node — mutation in one
// place is visible everywhere without rewalking, and JS handles can stay valid
// across structural edits provided the underlying slot is still occupied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeData {
    pub node_type: NodeType,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
}

// Arena-backed DOM. `nodes` is indexed by NodeId; `Option<NodeData>` reserves
// a tombstone for nodes that get removed later, so a stale handle whose slot
// has been freed observably resolves to `None` instead of pointing at a
// recycled element.
//
// `roots` mirrors the original `Vec<Node>` shape returned by the parser — the
// document is free to expose multiple top-level siblings (some test inputs
// have two), and consumers that only care about the first element walk
// `roots()[0]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    nodes: Vec<Option<NodeData>>,
    roots: Vec<NodeId>,
}

impl Document {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn roots(&self) -> &[NodeId] {
        &self.roots
    }

    pub fn get(&self, id: NodeId) -> Option<&NodeData> {
        self.nodes.get(id.index())?.as_ref()
    }

    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut NodeData> {
        self.nodes.get_mut(id.index())?.as_mut()
    }

    // Internal allocator. Builders below funnel through here so every newly
    // produced NodeId points at a fully-initialised slot.
    fn alloc(&mut self, data: NodeData) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(Some(data));
        id
    }

    pub fn create_element(&mut self, tag_name: impl Into<String>, attributes: AttrMap) -> NodeId {
        self.alloc(NodeData {
            node_type: NodeType::Element(ElementData {
                tag_name: tag_name.into(),
                attributes,
            }),
            parent: None,
            children: Vec::new(),
        })
    }

    pub fn create_text(&mut self, text: impl Into<String>) -> NodeId {
        self.alloc(NodeData {
            node_type: NodeType::Text(text.into()),
            parent: None,
            children: Vec::new(),
        })
    }

    /// Make `child` a top-level node of the document.
    pub fn append_root(&mut self, child: NodeId) {
        self.roots.push(child);
    }

    /// Wire `child` into `parent`'s children list and set the back-pointer.
    /// Panics on bogus ids — callers should only pass NodeIds the same arena
    /// just produced, so an invalid id signals a genuine bug rather than user
    /// input.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        let child_slot = self
            .get_mut(child)
            .expect("append_child: child id missing from arena");
        child_slot.parent = Some(parent);
        let parent_slot = self
            .get_mut(parent)
            .expect("append_child: parent id missing from arena");
        parent_slot.children.push(child);
    }

    pub fn element_data(&self, id: NodeId) -> Option<&ElementData> {
        match &self.get(id)?.node_type {
            NodeType::Element(e) => Some(e),
            NodeType::Text(_) => None,
        }
    }

    pub fn element_data_mut(&mut self, id: NodeId) -> Option<&mut ElementData> {
        match &mut self.get_mut(id)?.node_type {
            NodeType::Element(e) => Some(e),
            NodeType::Text(_) => None,
        }
    }

    pub fn text(&self, id: NodeId) -> Option<&str> {
        match &self.get(id)?.node_type {
            NodeType::Text(s) => Some(s.as_str()),
            NodeType::Element(_) => None,
        }
    }

    /// Unhook `id` from its current location — either a parent's children list
    /// or the root list. Used as a precondition by `appendChild`-style moves
    /// so the same node can be reparented without ending up in two places.
    /// No-op for an already-detached node and for a stale handle.
    pub fn detach(&mut self, id: NodeId) {
        let parent = self.get(id).and_then(|n| n.parent);
        match parent {
            Some(p) => {
                if let Some(parent_node) = self.get_mut(p) {
                    parent_node.children.retain(|c| *c != id);
                }
                if let Some(child) = self.get_mut(id) {
                    child.parent = None;
                }
            }
            None => {
                self.roots.retain(|r| *r != id);
            }
        }
    }

    /// Remove `child` from `parent`'s children list iff it is actually a
    /// direct child. Returns `true` when the unhook happens; callers handle
    /// stale handles / wrong-parent calls by inspecting the bool.
    pub fn remove_child(&mut self, parent: NodeId, child: NodeId) -> bool {
        let belongs = self
            .get(parent)
            .is_some_and(|p| p.children.contains(&child));
        if !belongs {
            return false;
        }
        if let Some(parent_node) = self.get_mut(parent) {
            parent_node.children.retain(|c| *c != child);
        }
        if let Some(child_node) = self.get_mut(child) {
            child_node.parent = None;
        }
        true
    }

    /// Tombstone `id` and every descendant — slots become `None`, so any
    /// outstanding NodeId handles to the freed nodes will resolve to `None`
    /// on the next `get`. Children are cleared first so the post-condition
    /// is "the entire subtree rooted at `id` is unreachable from the arena".
    pub fn tombstone_subtree(&mut self, id: NodeId) {
        let children: Vec<NodeId> = match self.get(id) {
            Some(node) => node.children.clone(),
            None => return,
        };
        for child in children {
            self.tombstone_subtree(child);
        }
        if let Some(slot) = self.nodes.get_mut(id.index()) {
            *slot = None;
        }
    }

    /// Drop every child of `id`, tombstone the freed subtrees, and replace
    /// them with a single text node carrying `text`. Mirrors the DOM
    /// `textContent =` setter: the element survives, its descendants don't.
    pub fn replace_with_text(&mut self, id: NodeId, text: String) {
        let kids: Vec<NodeId> = match self.get(id) {
            Some(node) => node.children.clone(),
            None => return,
        };
        if let Some(node) = self.get_mut(id) {
            node.children.clear();
        }
        for child in kids {
            self.tombstone_subtree(child);
        }
        let text_id = self.create_text(text);
        self.append_child(id, text_id);
    }

    /// Walk a `Vec<usize>` child-index path starting from a root. Returns
    /// `None` if any step indexes out of range — used by both the JS bridge
    /// and the legacy hover/focus path comparisons that store paths as the
    /// indices a child sits at within its parent's children list.
    pub fn resolve_path(&self, path: &[usize]) -> Option<NodeId> {
        let (first, rest) = path.split_first()?;
        let mut current = *self.roots.get(*first)?;
        for idx in rest {
            let node = self.get(current)?;
            current = *node.children.get(*idx)?;
        }
        Some(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_resolve_simple_tree() {
        let mut doc = Document::new();
        let mut attrs = AttrMap::new();
        attrs.insert("id".into(), "root".into());
        let root = doc.create_element("div", attrs);
        let text = doc.create_text("hi");
        doc.append_child(root, text);
        doc.append_root(root);

        assert_eq!(doc.roots(), &[root]);
        let root_node = doc.get(root).unwrap();
        assert_eq!(root_node.children, vec![text]);
        assert!(matches!(&root_node.node_type, NodeType::Element(_)));

        let text_node = doc.get(text).unwrap();
        assert_eq!(text_node.parent, Some(root));
        assert_eq!(doc.text(text), Some("hi"));
    }

    #[test]
    fn detach_unhooks_from_parent_and_clears_back_pointer() {
        let mut doc = Document::new();
        let parent = doc.create_element("div", AttrMap::new());
        let child = doc.create_element("p", AttrMap::new());
        doc.append_child(parent, child);
        doc.append_root(parent);

        doc.detach(child);

        assert!(doc.get(parent).unwrap().children.is_empty());
        assert_eq!(doc.get(child).unwrap().parent, None);
        // The slot itself is still alive — detach doesn't tombstone.
        assert!(doc.get(child).is_some());
    }

    #[test]
    fn detach_removes_from_roots_when_no_parent() {
        let mut doc = Document::new();
        let a = doc.create_element("a", AttrMap::new());
        let b = doc.create_element("b", AttrMap::new());
        doc.append_root(a);
        doc.append_root(b);
        doc.detach(a);
        assert_eq!(doc.roots(), &[b]);
    }

    #[test]
    fn remove_child_unhooks_only_when_child_belongs_to_parent() {
        let mut doc = Document::new();
        let parent = doc.create_element("div", AttrMap::new());
        let kid = doc.create_element("p", AttrMap::new());
        let stranger = doc.create_element("span", AttrMap::new());
        doc.append_child(parent, kid);

        // Stranger isn't a child — caller should observe the failure rather
        // than silently corrupt the tree by clearing parent's `kid` pointer.
        assert!(!doc.remove_child(parent, stranger));
        assert_eq!(doc.get(parent).unwrap().children, vec![kid]);

        assert!(doc.remove_child(parent, kid));
        assert!(doc.get(parent).unwrap().children.is_empty());
        assert_eq!(doc.get(kid).unwrap().parent, None);
    }

    #[test]
    fn tombstone_subtree_invalidates_descendants() {
        let mut doc = Document::new();
        let outer = doc.create_element("section", AttrMap::new());
        let inner = doc.create_element("p", AttrMap::new());
        let leaf = doc.create_text("x");
        doc.append_child(inner, leaf);
        doc.append_child(outer, inner);
        doc.append_root(outer);

        doc.tombstone_subtree(outer);
        // Every freed slot now resolves to None — that's the contract callers
        // (the JS bridge in particular) rely on for "stale handle" detection.
        assert!(doc.get(outer).is_none());
        assert!(doc.get(inner).is_none());
        assert!(doc.get(leaf).is_none());
    }

    #[test]
    fn replace_with_text_drops_descendants_and_appends_single_text_node() {
        let mut doc = Document::new();
        let host = doc.create_element("div", AttrMap::new());
        let stale_a = doc.create_element("p", AttrMap::new());
        let stale_b = doc.create_text("old");
        doc.append_child(stale_a, stale_b);
        doc.append_child(host, stale_a);
        doc.append_root(host);

        doc.replace_with_text(host, "fresh".to_string());

        // The old subtree is gone, both slots stale.
        assert!(doc.get(stale_a).is_none());
        assert!(doc.get(stale_b).is_none());
        // host now has exactly one text child carrying the new content.
        let host_node = doc.get(host).unwrap();
        assert_eq!(host_node.children.len(), 1);
        assert_eq!(doc.text(host_node.children[0]), Some("fresh"));
    }

    #[test]
    fn resolve_path_walks_child_indices() {
        let mut doc = Document::new();
        let outer = doc.create_element("section", AttrMap::new());
        let inner = doc.create_element("p", AttrMap::new());
        let leaf = doc.create_text("x");
        doc.append_child(inner, leaf);
        doc.append_child(outer, inner);
        doc.append_root(outer);

        // [0] = outer, [0,0] = inner, [0,0,0] = leaf
        assert_eq!(doc.resolve_path(&[0]), Some(outer));
        assert_eq!(doc.resolve_path(&[0, 0]), Some(inner));
        assert_eq!(doc.resolve_path(&[0, 0, 0]), Some(leaf));
        // Out-of-range indices produce None rather than panicking.
        assert_eq!(doc.resolve_path(&[0, 5]), None);
        assert_eq!(doc.resolve_path(&[]), None);
    }
}
