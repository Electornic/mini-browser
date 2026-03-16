use std::collections::BTreeMap;

pub type AttrMap = BTreeMap<String, String>;

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
    pub fn text(data: impl Into<String>) -> Self {
        Self {
            children: Vec::new(),
            node_type: NodeType::Text(data.into()),
        }
    }

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
