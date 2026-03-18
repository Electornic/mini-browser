use std::collections::BTreeMap;

// Attributes live in a deterministic map so debug output and snapshot-like tests stay stable.
pub type AttrMap = BTreeMap<String, String>;

// Node is the basic tree unit used everywhere after HTML parsing.
// Later stages never go back to the original HTML string; they only look at this tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub children: Vec<Node>,
    pub node_type: NodeType,
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

impl Node {
    // Text nodes are leaves in this toy browser, so they never have children.
    pub fn text(data: impl Into<String>) -> Self {
        Self {
            children: Vec::new(),
            node_type: NodeType::Text(data.into()),
        }
    }

    // Element nodes own their children directly because the parsed DOM stays immutable.
    // That keeps later stages simple: style/layout/render can treat the tree as read-only data.
    pub fn element(tag_name: impl Into<String>, attributes: AttrMap, children: Vec<Node>) -> Self {
        Self {
            children,
            node_type: NodeType::Element(ElementData {
                tag_name: tag_name.into(),
                attributes,
            }),
        }
    }
}
