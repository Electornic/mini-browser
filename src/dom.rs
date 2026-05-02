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

    /// Replace a Text node's data in place. Returns `false` for stale ids
    /// or when `id` points at an Element — the JS bridge uses the bool to
    /// distinguish "do nothing" from "throw".
    pub fn set_text(&mut self, id: NodeId, text: String) -> bool {
        match self.get_mut(id).map(|n| &mut n.node_type) {
            Some(NodeType::Text(s)) => {
                *s = text;
                true
            }
            _ => false,
        }
    }

    /// Insert `new_child` directly before `ref_child` in `parent`'s children
    /// list. Returns `false` when any id is stale or `ref_child` isn't a
    /// direct child of `parent`. `new_child` is detached from its current
    /// location first, so the same node can be moved (including reordered
    /// among existing siblings) without ending up in two places.
    pub fn insert_before(
        &mut self,
        parent: NodeId,
        new_child: NodeId,
        ref_child: NodeId,
    ) -> bool {
        if self.get(parent).is_none() || self.get(new_child).is_none() {
            return false;
        }
        let belongs = self
            .get(parent)
            .is_some_and(|p| p.children.contains(&ref_child));
        if !belongs {
            return false;
        }
        // Detach first; this can shift `ref_child`'s index when `new_child`
        // was already a sibling, so recompute the position afterwards.
        self.detach(new_child);
        let pos = self
            .get(parent)
            .and_then(|p| p.children.iter().position(|c| *c == ref_child))
            .expect("ref_child still present after detaching new_child");
        if let Some(child) = self.get_mut(new_child) {
            child.parent = Some(parent);
        }
        if let Some(p) = self.get_mut(parent) {
            p.children.insert(pos, new_child);
        }
        true
    }

    /// Replace `old_child` with `new_child` in `parent`'s children list.
    /// On success `old_child`'s subtree is tombstoned; `new_child` is
    /// detached from its current location first. Returns `false` for stale
    /// ids or when `old_child` isn't a direct child of `parent`.
    pub fn replace_child(
        &mut self,
        parent: NodeId,
        new_child: NodeId,
        old_child: NodeId,
    ) -> bool {
        if self.get(parent).is_none() || self.get(new_child).is_none() {
            return false;
        }
        let belongs = self
            .get(parent)
            .is_some_and(|p| p.children.contains(&old_child));
        if !belongs {
            return false;
        }
        self.detach(new_child);
        // `new_child` detach may have shifted indices when it was already
        // a sibling of `old_child`; recompute against the post-detach list.
        let pos = self
            .get(parent)
            .and_then(|p| p.children.iter().position(|c| *c == old_child))
            .expect("old_child still present after detaching new_child");
        if let Some(child) = self.get_mut(new_child) {
            child.parent = Some(parent);
        }
        if let Some(p) = self.get_mut(parent) {
            p.children[pos] = new_child;
        }
        if let Some(old) = self.get_mut(old_child) {
            old.parent = None;
        }
        self.tombstone_subtree(old_child);
        true
    }

    /// Recursively clone `id`. The returned NodeId is detached: it has no
    /// parent and is not in `roots` — callers wire it in via `append_child`
    /// / `insert_before` / `append_root`. With `deep = false` the clone has
    /// an empty children list; with `deep = true` the entire subtree is
    /// duplicated, each descendant getting its own fresh slot.
    pub fn clone_node(&mut self, id: NodeId, deep: bool) -> Option<NodeId> {
        let (data, kids) = {
            let node = self.get(id)?;
            let data = NodeData {
                node_type: node.node_type.clone(),
                parent: None,
                children: Vec::new(),
            };
            let kids = if deep {
                node.children.clone()
            } else {
                Vec::new()
            };
            (data, kids)
        };
        let new_id = self.alloc(data);
        for child in kids {
            if let Some(child_clone) = self.clone_node(child, true) {
                self.append_child(new_id, child_clone);
            }
        }
        Some(new_id)
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
    fn set_text_updates_text_node_in_place_and_rejects_elements() {
        let mut doc = Document::new();
        let text = doc.create_text("old");
        let elem = doc.create_element("div", AttrMap::new());

        assert!(doc.set_text(text, "new".to_string()));
        assert_eq!(doc.text(text), Some("new"));

        // Mutating an Element via set_text is a no-op + returns false so the
        // JS bridge can map that to a thrown TypeError instead of corrupting
        // the element into a text node.
        assert!(!doc.set_text(elem, "x".to_string()));
        assert!(matches!(
            doc.get(elem).unwrap().node_type,
            NodeType::Element(_)
        ));
    }

    #[test]
    fn insert_before_inserts_at_ref_child_position() {
        let mut doc = Document::new();
        let parent = doc.create_element("ul", AttrMap::new());
        let a = doc.create_element("li", AttrMap::new());
        let b = doc.create_element("li", AttrMap::new());
        let c = doc.create_element("li", AttrMap::new());
        let new_kid = doc.create_element("li", AttrMap::new());
        doc.append_child(parent, a);
        doc.append_child(parent, b);
        doc.append_child(parent, c);

        assert!(doc.insert_before(parent, new_kid, b));
        assert_eq!(doc.get(parent).unwrap().children, vec![a, new_kid, b, c]);
        assert_eq!(doc.get(new_kid).unwrap().parent, Some(parent));
    }

    #[test]
    fn insert_before_reorders_existing_sibling_correctly() {
        // The sibling case is the one most likely to mis-index: detaching
        // `c` shifts the position of `a` from 0 → 0 (unchanged here), but
        // a naive implementation that captured the position before detach
        // would crash if we'd moved a later sibling forward.
        let mut doc = Document::new();
        let parent = doc.create_element("ul", AttrMap::new());
        let a = doc.create_element("li", AttrMap::new());
        let b = doc.create_element("li", AttrMap::new());
        let c = doc.create_element("li", AttrMap::new());
        doc.append_child(parent, a);
        doc.append_child(parent, b);
        doc.append_child(parent, c);

        // Move c to before a.
        assert!(doc.insert_before(parent, c, a));
        assert_eq!(doc.get(parent).unwrap().children, vec![c, a, b]);
    }

    #[test]
    fn insert_before_returns_false_when_ref_is_not_a_child() {
        let mut doc = Document::new();
        let parent = doc.create_element("div", AttrMap::new());
        let kid = doc.create_element("p", AttrMap::new());
        let stranger = doc.create_element("span", AttrMap::new());
        let new_kid = doc.create_element("em", AttrMap::new());
        doc.append_child(parent, kid);

        assert!(!doc.insert_before(parent, new_kid, stranger));
        // The new node was NOT inserted (still detached).
        assert_eq!(doc.get(parent).unwrap().children, vec![kid]);
        assert_eq!(doc.get(new_kid).unwrap().parent, None);
    }

    #[test]
    fn replace_child_swaps_subtree_and_tombstones_old() {
        let mut doc = Document::new();
        let parent = doc.create_element("section", AttrMap::new());
        let old = doc.create_element("p", AttrMap::new());
        let old_kid = doc.create_text("gone");
        doc.append_child(old, old_kid);
        doc.append_child(parent, old);
        let fresh = doc.create_element("p", AttrMap::new());

        assert!(doc.replace_child(parent, fresh, old));
        assert_eq!(doc.get(parent).unwrap().children, vec![fresh]);
        assert_eq!(doc.get(fresh).unwrap().parent, Some(parent));
        // Old subtree is fully tombstoned.
        assert!(doc.get(old).is_none());
        assert!(doc.get(old_kid).is_none());
    }

    #[test]
    fn clone_node_shallow_copies_only_the_node_itself() {
        let mut doc = Document::new();
        let mut attrs = AttrMap::new();
        attrs.insert("id".into(), "src".into());
        let src = doc.create_element("div", attrs);
        let kid = doc.create_text("hi");
        doc.append_child(src, kid);

        let dup = doc.clone_node(src, false).unwrap();
        // Same tag + attributes, but no children and parent=None.
        let dup_data = doc.element_data(dup).unwrap();
        assert_eq!(dup_data.tag_name, "div");
        assert_eq!(dup_data.attributes.get("id").map(|s| s.as_str()), Some("src"));
        let dup_node = doc.get(dup).unwrap();
        assert!(dup_node.children.is_empty());
        assert_eq!(dup_node.parent, None);
        // Original is untouched.
        assert_eq!(doc.get(src).unwrap().children, vec![kid]);
    }

    #[test]
    fn clone_node_deep_duplicates_descendants_into_fresh_slots() {
        let mut doc = Document::new();
        let src = doc.create_element("ul", AttrMap::new());
        let li = doc.create_element("li", AttrMap::new());
        let txt = doc.create_text("one");
        doc.append_child(li, txt);
        doc.append_child(src, li);

        let dup = doc.clone_node(src, true).unwrap();
        // The new subtree mirrors the structure but uses fresh ids.
        let dup_kids = &doc.get(dup).unwrap().children;
        assert_eq!(dup_kids.len(), 1);
        let dup_li = dup_kids[0];
        assert_ne!(dup_li, li);
        let dup_txt_id = doc.get(dup_li).unwrap().children[0];
        assert_ne!(dup_txt_id, txt);
        assert_eq!(doc.text(dup_txt_id), Some("one"));
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
