use std::collections::BTreeMap;

use crate::{
    css::{Declaration, Selector, SimpleSelector, Stylesheet, Unit, Value},
    dom::{ElementData, Node, NodeType},
};

pub type PropertyMap = BTreeMap<String, Value>;

// Mirrors a real browser's user-agent default for fonts. Used as the baseline whenever
// no font-size is in scope (root with no font-size declaration, em/rem on the root, etc.).
const DEFAULT_FONT_SIZE: f32 = 16.0;

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
    // The root font-size feeds rem resolution for every descendant. Compute it up front
    // by treating the root as if it lived inside the user-agent default font-size.
    let raw_root = specified_values(root, stylesheets, &[]);
    let root_font_size = resolve_font_size(
        raw_root.get("font-size"),
        DEFAULT_FONT_SIZE,
        DEFAULT_FONT_SIZE,
    );
    style_tree_with_parent(root, stylesheets, None, root_font_size, &[])
}

fn style_tree_with_parent<'a>(
    node: &'a Node,
    stylesheets: &[Stylesheet],
    parent_values: Option<&PropertyMap>,
    root_font_size: f32,
    ancestors: &[&'a Node],
) -> StyledNode {
    let mut specified_values = specified_values(node, stylesheets, ancestors);

    // Real browsers inherit many properties. Here we only inherit a few text-related ones
    // because they make documents readable without making the style system much more complex.
    for property in ["color", "font-size", "text-align"] {
        if !specified_values.contains_key(property)
            && let Some(value) = parent_values.and_then(|values| values.get(property))
        {
            specified_values.insert(property.to_string(), value.clone());
        }
    }

    // Font-size is resolved first because every other em-based length on this node depends
    // on it. Parent font-size has already been resolved to Px during the parent's pass, so
    // looking it up here is a straightforward read.
    let parent_font_size = parent_values
        .and_then(|values| values.get("font-size"))
        .and_then(|value| match value {
            Value::Length(v, Unit::Px) => Some(*v),
            _ => None,
        })
        .unwrap_or(DEFAULT_FONT_SIZE);
    let own_font_size = resolve_font_size(
        specified_values.get("font-size"),
        parent_font_size,
        root_font_size,
    );

    // Replace own font-size with the resolved Px value so descendants see the cascaded
    // value, then resolve every other em/rem to Px in place. Percent stays untouched —
    // it depends on layout-time containing-block dimensions.
    specified_values.insert("font-size".into(), Value::Length(own_font_size, Unit::Px));
    for value in specified_values.values_mut() {
        match value {
            Value::Length(v, Unit::Em) => *value = Value::Length(*v * own_font_size, Unit::Px),
            Value::Length(v, Unit::Rem) => *value = Value::Length(*v * root_font_size, Unit::Px),
            _ => {}
        }
    }

    // Append self to the ancestor chain children see during their selector matching.
    let mut child_ancestors: Vec<&Node> = ancestors.to_vec();
    child_ancestors.push(node);
    let children = node
        .children
        .iter()
        .map(|child| {
            style_tree_with_parent(
                child,
                stylesheets,
                Some(&specified_values),
                root_font_size,
                &child_ancestors,
            )
        })
        .collect();

    StyledNode {
        node: node.clone(),
        specified_values,
        children,
    }
}

fn resolve_font_size(raw: Option<&Value>, parent_font_size: f32, root_font_size: f32) -> f32 {
    match raw {
        Some(Value::Length(v, Unit::Px)) => *v,
        Some(Value::Length(v, Unit::Em)) => *v * parent_font_size,
        Some(Value::Length(v, Unit::Rem)) => *v * root_font_size,
        // CSS spec resolves font-size: <percent> against the parent's font-size, just like em.
        Some(Value::Length(v, Unit::Percent)) => *v / 100.0 * parent_font_size,
        _ => parent_font_size,
    }
}

fn specified_values(node: &Node, stylesheets: &[Stylesheet], ancestors: &[&Node]) -> PropertyMap {
    let mut matched = Vec::new();

    // First collect every rule that matches this node together with its specificity and order.
    for (rule_order, rule) in stylesheets
        .iter()
        .flat_map(|sheet| sheet.rules.iter())
        .enumerate()
    {
        if let Some(specificity) = matching_specificity(node, ancestors, &rule.selectors) {
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

fn matching_specificity(node: &Node, ancestors: &[&Node], selectors: &[Selector]) -> Option<u32> {
    // The highest matching selector wins within a rule group such as `h1, .title`.
    selectors
        .iter()
        .filter(|selector| matches_selector(node, ancestors, selector))
        .map(selector_specificity)
        .max()
}

fn selector_specificity(selector: &Selector) -> u32 {
    // Sum specificity across the whole chain so descendant selectors will already give the
    // right answer once the parser starts emitting them.
    selector.parts.iter().map(simple_specificity).sum()
}

fn simple_specificity(simple: &SimpleSelector) -> u32 {
    match simple {
        SimpleSelector::Tag(_) => 1,
        SimpleSelector::Class(_) => 10,
        SimpleSelector::Id(_) => 100,
    }
}

fn matches_selector(node: &Node, ancestors: &[&Node], selector: &Selector) -> bool {
    // Right-to-left matching: the rightmost simple selector is the target and must match
    // the element being styled. The remaining parts have to appear in DOM order somewhere
    // along the ancestor chain. The walk is greedy — it consumes the closest matching
    // ancestor — which is fine for the descendant combinator because all parts only need
    // *some* ancestor to satisfy them, not a specific one.
    let Some((target, rest)) = selector.parts.split_last() else {
        return false;
    };
    if !matches_simple(node, target) {
        return false;
    }

    let mut ancestor_iter = ancestors.iter().rev();
    for part in rest.iter().rev() {
        loop {
            match ancestor_iter.next() {
                Some(ancestor) if matches_simple(ancestor, part) => break,
                Some(_) => continue,
                None => return false,
            }
        }
    }
    true
}

fn matches_simple(node: &Node, simple: &SimpleSelector) -> bool {
    let element = match &node.node_type {
        NodeType::Element(element) => element,
        // Text nodes never match selectors directly; they only inherit style from parents.
        NodeType::Text(_) => return false,
    };

    match simple {
        SimpleSelector::Tag(tag_name) => element.tag_name == *tag_name,
        SimpleSelector::Class(class_name) => has_class(element, class_name),
        SimpleSelector::Id(id) => element
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
    fn descendant_selector_matches_nested_target() {
        let root = parse_html(
            r#"<div class="outer"><section><span class="inner">hi</span></section></div>"#,
        );
        let stylesheet = parse_css(".outer .inner { color: #ff0000; }");
        let styled = style::style_tree(&root, &[stylesheet]);
        let inner = &styled.children[0].children[0];

        // .outer .inner targets the <span>, even though a <section> sits between them.
        assert_eq!(
            inner.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn descendant_selector_does_not_match_when_ancestor_is_missing() {
        let root = parse_html(r#"<div><span class="inner">hi</span></div>"#);
        let stylesheet = parse_css(".outer .inner { color: #ff0000; }");
        let styled = style::style_tree(&root, &[stylesheet]);
        let inner = &styled.children[0];

        // No `.outer` ancestor exists, so the rule must not apply.
        assert_eq!(inner.value("color"), None);
    }

    #[test]
    fn descendant_selector_specificity_sums_across_chain() {
        // .outer .inner has specificity 10 + 10 = 20, beating the lone .inner rule (10)
        // even when the latter is listed later in the stylesheet.
        let root = parse_html(r#"<div class="outer"><span class="inner">hi</span></div>"#);
        let stylesheet = parse_css(
            r#"
                .outer .inner { color: #ff0000; }
                .inner { color: #00ff00; }
            "#,
        );
        let styled = style::style_tree(&root, &[stylesheet]);
        let inner = &styled.children[0];

        assert_eq!(
            inner.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn em_resolves_against_parent_font_size_for_font_size_itself() {
        let root = parse_html(r#"<div id="outer"><div id="inner"></div></div>"#);
        let stylesheet = parse_css(
            r#"
                #outer { font-size: 20px; }
                #inner { font-size: 1.5em; }
            "#,
        );
        let styled = style::style_tree(&root, &[stylesheet]);
        let inner = &styled.children[0];

        // 1.5em on a 20px parent resolves to 30px and is stored as a Px length so children
        // see it during their own cascade.
        assert_eq!(
            inner.value("font-size"),
            Some(&Value::Length(30.0, Unit::Px))
        );
    }

    #[test]
    fn em_on_other_properties_uses_own_resolved_font_size() {
        let root = parse_html(r#"<div id="outer"><div id="inner"></div></div>"#);
        let stylesheet = parse_css(
            r#"
                #outer { font-size: 20px; }
                #inner { font-size: 1.5em; padding-left: 2em; }
            "#,
        );
        let styled = style::style_tree(&root, &[stylesheet]);
        let inner = &styled.children[0];

        // padding 2em uses inner's resolved font-size (30px), not the parent's: 60px.
        assert_eq!(
            inner.value("padding-left"),
            Some(&Value::Length(60.0, Unit::Px))
        );
    }

    #[test]
    fn rem_resolves_against_root_font_size_regardless_of_depth() {
        let root = parse_html(
            r#"<div id="root"><div class="middle"><div class="leaf"></div></div></div>"#,
        );
        let stylesheet = parse_css(
            r#"
                #root { font-size: 24px; }
                .middle { font-size: 12px; }
                .leaf { padding-left: 0.5rem; }
            "#,
        );
        let styled = style::style_tree(&root, &[stylesheet]);
        let leaf = &styled.children[0].children[0];

        // 0.5rem references the root font-size (24px), independent of the .middle ancestor.
        assert_eq!(
            leaf.value("padding-left"),
            Some(&Value::Length(12.0, Unit::Px))
        );
    }

    #[test]
    fn percent_on_non_font_properties_stays_unresolved_until_layout() {
        let root = parse_html(r#"<div class="card"></div>"#);
        let stylesheet = parse_css(".card { width: 50%; }");
        let styled = style::style_tree(&root, &[stylesheet]);

        // Percent on width is held back for the layout layer to resolve against the
        // containing block's content width.
        assert_eq!(
            styled.value("width"),
            Some(&Value::Length(50.0, Unit::Percent))
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
