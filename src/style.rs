use std::collections::BTreeMap;

use crate::{
    css::{Declaration, Selector, Stylesheet, Value},
    dom::{ElementData, Node, NodeType},
};

pub type PropertyMap = BTreeMap<String, Value>;

// StyledNode mirrors the DOM tree but replaces raw attributes with resolved CSS properties.
// If you want to know "what style does this node end up with?", this is the structure to inspect.
#[derive(Debug, Clone, PartialEq)]
pub struct StyledNode {
    pub node: Node,
    pub specified_values: PropertyMap,
    pub children: Vec<StyledNode>,
}

impl StyledNode {
    pub fn value(&self, name: &str) -> Option<&Value> {
        self.specified_values.get(name)
    }
}

pub fn style_tree(root: &Node, stylesheets: &[Stylesheet]) -> StyledNode {
    style_tree_with_parent(root, stylesheets, None)
}

fn style_tree_with_parent(
    node: &Node,
    stylesheets: &[Stylesheet],
    parent_values: Option<&PropertyMap>,
) -> StyledNode {
    let mut specified_values = specified_values(node, stylesheets);

    // Real browsers inherit many properties. Here we only inherit a few text-related ones
    // because they make documents readable without making the style system much more complex.
    for property in ["color", "font-size"] {
        if !specified_values.contains_key(property) {
            if let Some(value) = parent_values.and_then(|values| values.get(property)) {
                specified_values.insert(property.to_string(), value.clone());
            }
        }
    }

    let children = node
        .children
        .iter()
        .map(|child| style_tree_with_parent(child, stylesheets, Some(&specified_values)))
        .collect();

    StyledNode {
        node: node.clone(),
        specified_values,
        children,
    }
}

fn specified_values(node: &Node, stylesheets: &[Stylesheet]) -> PropertyMap {
    let mut matched = Vec::new();

    // First collect every rule that matches this node together with its specificity and order.
    for (rule_order, rule) in stylesheets
        .iter()
        .flat_map(|sheet| sheet.rules.iter())
        .enumerate()
    {
        if let Some(specificity) = matching_specificity(node, &rule.selectors) {
            matched.push((specificity, rule_order, &rule.declarations));
        }
    }

    // Lower-priority rules are applied first so later, more specific matches overwrite them.
    matched.sort_by_key(|(specificity, rule_order, _)| (*specificity, *rule_order));

    let mut values = default_values(node);
    for (_, _, declarations) in matched {
        apply_declarations(&mut values, declarations);
    }

    values
}

fn default_values(node: &Node) -> PropertyMap {
    let mut values = PropertyMap::new();
    let element = match &node.node_type {
        NodeType::Element(element) => element,
        NodeType::Text(_) => return values,
    };

    // These defaults act like a tiny user-agent stylesheet so unstyled pages remain legible.
    match element.tag_name.as_str() {
        "body" => {
            edge_defaults(&mut values, "margin", 8.0);
        }
        "p" => {
            values.insert(
                "margin-top".into(),
                Value::Length(12.0, crate::css::Unit::Px),
            );
            values.insert(
                "margin-bottom".into(),
                Value::Length(12.0, crate::css::Unit::Px),
            );
        }
        "h1" => {
            values.insert(
                "font-size".into(),
                Value::Length(32.0, crate::css::Unit::Px),
            );
            values.insert(
                "margin-top".into(),
                Value::Length(12.0, crate::css::Unit::Px),
            );
            values.insert(
                "margin-bottom".into(),
                Value::Length(16.0, crate::css::Unit::Px),
            );
        }
        "a" => {
            values.insert(
                "color".into(),
                Value::Color(crate::css::Color {
                    r: 0,
                    g: 102,
                    b: 204,
                    a: 255,
                }),
            );
        }
        _ => {}
    }

    values
}

fn edge_defaults(values: &mut PropertyMap, prefix: &str, amount: f32) {
    for side in ["top", "right", "bottom", "left"] {
        values.insert(
            format!("{prefix}-{side}"),
            Value::Length(amount, crate::css::Unit::Px),
        );
    }
}

fn apply_declarations(values: &mut PropertyMap, declarations: &[Declaration]) {
    // Later declarations with the same property name overwrite earlier ones.
    for declaration in declarations {
        values.insert(declaration.name.clone(), declaration.value.clone());
    }
}

fn matching_specificity(node: &Node, selectors: &[Selector]) -> Option<u32> {
    // The highest matching selector wins within a rule group such as `h1, .title`.
    selectors
        .iter()
        .filter(|selector| matches_selector(node, selector))
        .map(selector_specificity)
        .max()
}

fn selector_specificity(selector: &Selector) -> u32 {
    match selector {
        Selector::Tag(_) => 1,
        Selector::Class(_) => 10,
        Selector::Id(_) => 100,
    }
}

fn matches_selector(node: &Node, selector: &Selector) -> bool {
    let element = match &node.node_type {
        NodeType::Element(element) => element,
        // Text nodes never match selectors directly; they only inherit style from parents.
        NodeType::Text(_) => return false,
    };

    match selector {
        Selector::Tag(tag_name) => element.tag_name == *tag_name,
        Selector::Class(class_name) => has_class(element, class_name),
        Selector::Id(id) => element
            .attributes
            .get("id")
            .is_some_and(|value| value == id),
    }
}

fn has_class(element: &ElementData, class_name: &str) -> bool {
    element
        .attributes
        .get("class")
        .is_some_and(|value| value.split_whitespace().any(|class| class == class_name))
}

#[cfg(test)]
mod tests {
    use crate::{
        css::{Color, Unit, Value},
        html, style,
    };

    fn parse_html(source: &str) -> crate::dom::Node {
        html::parse(source).unwrap().into_iter().next().unwrap()
    }

    fn parse_css(source: &str) -> crate::css::Stylesheet {
        crate::css::parse(source).unwrap()
    }

    #[test]
    fn applies_rule_specificity_in_tag_class_id_order() {
        let root = parse_html(r#"<div id="hero" class="card promo">Hello</div>"#);
        let stylesheet = parse_css(
            r#"
                div { color: #111111; display: block; }
                .promo { color: #222222; }
                #hero { color: #333333; }
            "#,
        );

        let styled = style::style_tree(&root, &[stylesheet]);

        assert_eq!(
            styled.value("color"),
            Some(&Value::Color(Color {
                r: 51,
                g: 51,
                b: 51,
                a: 255,
            }))
        );
        assert_eq!(
            styled.value("display"),
            Some(&Value::Keyword("block".into()))
        );
    }

    #[test]
    fn inherits_color_and_font_size_from_parent() {
        let root = parse_html(r#"<div id="app"><span>Text</span></div>"#);
        let stylesheet = parse_css(
            r#"
                #app {
                    color: #ff0000;
                    font-size: 18px;
                }
            "#,
        );

        let styled = style::style_tree(&root, &[stylesheet]);
        let child = &styled.children[0];

        assert_eq!(
            child.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
        assert_eq!(
            child.value("font-size"),
            Some(&Value::Length(18.0, Unit::Px))
        );
    }

    #[test]
    fn text_nodes_inherit_parent_style() {
        let root = parse_html(r#"<p class="copy">Hello</p>"#);
        let stylesheet = parse_css(
            r#"
                .copy {
                    color: #0f0;
                }
            "#,
        );

        let styled = style::style_tree(&root, &[stylesheet]);
        let text = &styled.children[0];

        assert_eq!(
            text.value("color"),
            Some(&Value::Color(Color {
                r: 0,
                g: 255,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn applies_basic_user_agent_defaults() {
        let root = parse_html(r#"<body><h1>Title</h1><p>Copy</p><a href="/next">Next</a></body>"#);
        let styled = style::style_tree(&root, &[]);

        assert_eq!(
            styled.value("margin-top"),
            Some(&Value::Length(8.0, Unit::Px))
        );
        assert_eq!(
            styled.children[0].value("font-size"),
            Some(&Value::Length(32.0, Unit::Px))
        );
        assert_eq!(
            styled.children[1].value("margin-bottom"),
            Some(&Value::Length(12.0, Unit::Px))
        );
        assert_eq!(
            styled.children[2].value("color"),
            Some(&Value::Color(Color {
                r: 0,
                g: 102,
                b: 204,
                a: 255,
            }))
        );
    }

    #[test]
    fn author_styles_override_user_agent_defaults() {
        let root = parse_html(r#"<body><a href="/next">Next</a></body>"#);
        let stylesheet = parse_css(
            r#"
                body { margin-top: 20px; }
                a { color: #ff0000; }
            "#,
        );
        let styled = style::style_tree(&root, &[stylesheet]);

        assert_eq!(
            styled.value("margin-top"),
            Some(&Value::Length(20.0, Unit::Px))
        );
        assert_eq!(
            styled.children[0].value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }
}
