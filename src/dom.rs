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

    pub fn text(&self, id: NodeId) -> Option<&str> {
        match &self.get(id)?.node_type {
            NodeType::Text(s) => Some(s.as_str()),
            NodeType::Element(_) => None,
        }
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
