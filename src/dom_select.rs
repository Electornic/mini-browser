// Bridge between our DOM arena and the `selectors` crate.
//
// `MatchingElement` is the lightweight wrapper the matching engine calls back
// into for tree traversal (`parent_element`, `prev_sibling_element`, …),
// attribute lookup (`has_id`, `has_class`, `attr_matches`), and runtime
// pseudo-class state (`:hover`, `:focus`, `:active`, `:link`, `:visited`).
//
// The wrapper is `Copy`-shaped so the matcher can clone it for every
// ancestor walk without per-call allocation; it borrows the live `Document`
// and a `MatchingState` that pins down "which engaged node is hovered /
// focused / active" once at the start of a styling pass.

use selectors::OpaqueElement;
use selectors::attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint};
use selectors::bloom::BloomFilter;
use selectors::context::MatchingContext;
use selectors::matching::ElementSelectorFlags;

use crate::css::{CssString, MiniBrowserSelectorImpl, NonTSPseudoClass};
use crate::dom::{Document, NodeId, NodeType};

/// Pre-resolved engaged-element ids for a single styling pass. The cascade
/// resolves the user-supplied `InteractionState` paths into NodeIds once,
/// then hands `MatchingState` to every per-node matching call so the
/// pseudo-class predicates are O(ancestor_walk) rather than O(path_compare).
#[derive(Default, Copy, Clone, Debug)]
pub struct MatchingState {
    /// NodeId of the deepest hovered element (the cursor's leaf). `:hover`
    /// also matches every ancestor on the way down per CSS spec, so the
    /// pseudo-class check walks self → root looking for this id.
    pub hover: Option<NodeId>,
    /// NodeId of the focused element. `:focus` does not propagate to
    /// ancestors, so the pseudo-class check is identity-equality.
    pub focus: Option<NodeId>,
    /// NodeId of the deepest active element (mouse-pressed leaf). Same
    /// ancestor-propagation rule as `:hover`.
    pub active: Option<NodeId>,
}

#[derive(Clone, Copy)]
pub struct MatchingElement<'a> {
    pub id: NodeId,
    pub doc: &'a Document,
    pub state: &'a MatchingState,
}

impl std::fmt::Debug for MatchingElement<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MatchingElement")
            .field("id", &self.id)
            .finish()
    }
}

impl<'a> MatchingElement<'a> {
    pub fn new(id: NodeId, doc: &'a Document, state: &'a MatchingState) -> Self {
        Self { id, doc, state }
    }

    fn with_id(&self, id: NodeId) -> Self {
        Self {
            id,
            doc: self.doc,
            state: self.state,
        }
    }

    fn parent_id(&self) -> Option<NodeId> {
        self.doc.get(self.id)?.parent
    }

    fn tag_name(&self) -> Option<&'a str> {
        match &self.doc.get(self.id)?.node_type {
            NodeType::Element(e) => Some(e.tag_name.as_str()),
            NodeType::Text(_) => None,
        }
    }
}

impl<'a> selectors::Element for MatchingElement<'a> {
    type Impl = MiniBrowserSelectorImpl;

    fn opaque(&self) -> OpaqueElement {
        // The selectors crate uses opaque pointer identity for `:scope` and
        // similar features; we don't model those, but the trait still
        // requires a stable, comparable token. Hashing `(doc as *const, id)`
        // would be ideal but `OpaqueElement::new` takes any reference, so
        // we feed it a stable per-id-per-document address by indexing
        // into the document's nodes vec via the id's raw value as offset.
        // Falling back to a static address keeps the API requirement met
        // even when the slot has been tombstoned.
        match self.doc.get(self.id) {
            Some(node) => OpaqueElement::new(node),
            None => OpaqueElement::new(self.doc),
        }
    }

    fn parent_element(&self) -> Option<Self> {
        self.parent_id().map(|p| self.with_id(p))
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        false
    }

    fn containing_shadow_host(&self) -> Option<Self> {
        None
    }

    fn is_pseudo_element(&self) -> bool {
        false
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        let parent = self.parent_id()?;
        let siblings = &self.doc.get(parent)?.children;
        let pos = siblings.iter().position(|c| *c == self.id)?;
        // Walk backwards skipping non-element nodes (text). The selectors
        // crate uses this for adjacent (`+`) and general-sibling (`~`)
        // combinators; both want the previous *element*, not text.
        for &candidate in siblings[..pos].iter().rev() {
            if matches!(
                self.doc.get(candidate).map(|n| &n.node_type),
                Some(NodeType::Element(_))
            ) {
                return Some(self.with_id(candidate));
            }
        }
        None
    }

    fn next_sibling_element(&self) -> Option<Self> {
        let parent = self.parent_id()?;
        let siblings = &self.doc.get(parent)?.children;
        let pos = siblings.iter().position(|c| *c == self.id)?;
        for &candidate in siblings[pos + 1..].iter() {
            if matches!(
                self.doc.get(candidate).map(|n| &n.node_type),
                Some(NodeType::Element(_))
            ) {
                return Some(self.with_id(candidate));
            }
        }
        None
    }

    fn first_element_child(&self) -> Option<Self> {
        for &child in &self.doc.get(self.id)?.children {
            if matches!(
                self.doc.get(child).map(|n| &n.node_type),
                Some(NodeType::Element(_))
            ) {
                return Some(self.with_id(child));
            }
        }
        None
    }

    fn is_html_element_in_html_document(&self) -> bool {
        // Every element we model is HTML; quirks-mode case sensitivity
        // for class/id selectors flows through `MatchingContext` rather
        // than this flag, but the trait still wants an honest answer.
        true
    }

    fn has_local_name(&self, local_name: &str) -> bool {
        match self.tag_name() {
            Some(tag) => tag.eq_ignore_ascii_case(local_name),
            None => false,
        }
    }

    fn has_namespace(&self, ns: &str) -> bool {
        // Empty namespace == "no namespace", which is the only case we
        // model (HTML5 has only one default namespace).
        ns.is_empty()
    }

    fn is_same_type(&self, other: &Self) -> bool {
        match (self.tag_name(), other.tag_name()) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        }
    }

    fn attr_matches(
        &self,
        ns: &NamespaceConstraint<&CssString>,
        local_name: &CssString,
        operation: &AttrSelectorOperation<&CssString>,
    ) -> bool {
        // We don't model namespaces, so any namespace constraint other
        // than "no namespace" / "any namespace" misses.
        match ns {
            NamespaceConstraint::Any => {}
            NamespaceConstraint::Specific(url) if url.as_str().is_empty() => {}
            NamespaceConstraint::Specific(_) => return false,
        }
        let Some(elem) = self.doc.element_data(self.id) else {
            return false;
        };
        let Some(value) = elem.attributes.get(local_name.as_str()) else {
            return false;
        };
        operation.eval_str(value)
    }

    fn match_non_ts_pseudo_class(
        &self,
        pc: &NonTSPseudoClass,
        _context: &mut MatchingContext<Self::Impl>,
    ) -> bool {
        let Some(elem) = self.doc.element_data(self.id) else {
            return false;
        };
        match pc {
            NonTSPseudoClass::Hover => {
                Self::ancestor_chain_engaged(self.doc, self.state.hover, self.id)
            }
            NonTSPseudoClass::Active => {
                Self::ancestor_chain_engaged(self.doc, self.state.active, self.id)
            }
            // Focus is tied to the focused element itself — no propagation.
            NonTSPseudoClass::Focus => self.state.focus == Some(self.id),
            // Without a visited set we treat every `<a href>` as unvisited
            // (the fresh-eyes default), which mirrors what the previous
            // hand-rolled matcher exposed.
            NonTSPseudoClass::Link => {
                elem.tag_name.eq_ignore_ascii_case("a") && elem.attributes.contains_key("href")
            }
            NonTSPseudoClass::Visited => false,
        }
    }

    fn match_pseudo_element(
        &self,
        _pe: &<Self::Impl as selectors::SelectorImpl>::PseudoElement,
        _context: &mut MatchingContext<Self::Impl>,
    ) -> bool {
        // Our PseudoElement is the empty enum, so this can never be reached.
        false
    }

    fn apply_selector_flags(&self, _flags: ElementSelectorFlags) {
        // Flags drive Servo's invalidation tracking; we restyle from
        // scratch each frame, so we ignore them.
    }

    fn is_link(&self) -> bool {
        let Some(elem) = self.doc.element_data(self.id) else {
            return false;
        };
        elem.tag_name.eq_ignore_ascii_case("a") && elem.attributes.contains_key("href")
    }

    fn is_html_slot_element(&self) -> bool {
        false
    }

    fn has_id(&self, id: &CssString, case_sensitivity: CaseSensitivity) -> bool {
        let Some(elem) = self.doc.element_data(self.id) else {
            return false;
        };
        match elem.attributes.get("id") {
            Some(value) => case_sensitivity.eq(value.as_bytes(), id.as_str().as_bytes()),
            None => false,
        }
    }

    fn has_class(&self, name: &CssString, case_sensitivity: CaseSensitivity) -> bool {
        let Some(elem) = self.doc.element_data(self.id) else {
            return false;
        };
        match elem.attributes.get("class") {
            Some(value) => value
                .split_whitespace()
                .any(|cls| case_sensitivity.eq(cls.as_bytes(), name.as_str().as_bytes())),
            None => false,
        }
    }

    fn has_custom_state(&self, _name: &CssString) -> bool {
        false
    }

    fn imported_part(&self, _name: &CssString) -> Option<CssString> {
        None
    }

    fn is_part(&self, _name: &CssString) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        let Some(node) = self.doc.get(self.id) else {
            return true;
        };
        node.children.iter().all(|child| {
            match self.doc.get(*child).map(|n| &n.node_type) {
                Some(NodeType::Element(_)) => false,
                Some(NodeType::Text(s)) => s.is_empty(),
                None => true,
            }
        })
    }

    fn is_root(&self) -> bool {
        // The arena exposes a roots list; a node is the document root when
        // it has no parent and is the very first root entry.
        self.doc.get(self.id).is_some_and(|n| n.parent.is_none())
            && self.doc.roots().first() == Some(&self.id)
    }

    fn add_element_unique_hashes(&self, _filter: &mut BloomFilter) -> bool {
        // We don't run the bloom-filter fast-reject path (we don't construct
        // an `AncestorHashes` per selector), so contributing nothing is fine.
        false
    }
}

impl MatchingElement<'_> {
    /// Mirrors `is_engaged` but as a free helper so it doesn't borrow `self`.
    fn ancestor_chain_engaged(doc: &Document, target: Option<NodeId>, candidate: NodeId) -> bool {
        let Some(target) = target else { return false };
        let mut cur = target;
        loop {
            if cur == candidate {
                return true;
            }
            match doc.get(cur).and_then(|n| n.parent) {
                Some(p) => cur = p,
                None => return false,
            }
        }
    }
}
