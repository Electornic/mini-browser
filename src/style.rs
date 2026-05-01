use std::collections::BTreeMap;

use crate::{
    css::{
        Combinator, Declaration, PseudoClass, Selector, SimpleSelector, SimpleSelectorKind,
        Stylesheet, Unit, Value,
    },
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

/// User-facing interaction state passed into `style_tree_with_state`. Each field is the
/// DOM path (sequence of child indices from the root) of the corresponding node, or
/// `None` if the state is not active.
#[derive(Default, Copy, Clone, Debug)]
pub struct InteractionState<'a> {
    pub hover: Option<&'a [usize]>,
    pub focus: Option<&'a [usize]>,
    pub active: Option<&'a [usize]>,
}

/// Per-node pseudo-class state used during selector matching.
#[derive(Default, Copy, Clone, Debug)]
struct PseudoState {
    hover: bool,
    focus: bool,
    active: bool,
}

pub fn style_tree(root: &Node, stylesheets: &[Stylesheet]) -> StyledNode {
    // Most callers do not care about interaction state — they get a "nothing engaged" tree.
    style_tree_with_state(root, stylesheets, InteractionState::default())
}

/// Backward-compatible convenience: same as `style_tree_with_state` with only the
/// hovered path filled in. New callers should reach for `style_tree_with_state` directly.
pub fn style_tree_with_hover(
    root: &Node,
    stylesheets: &[Stylesheet],
    hovered_path: Option<&[usize]>,
) -> StyledNode {
    style_tree_with_state(
        root,
        stylesheets,
        InteractionState {
            hover: hovered_path,
            ..Default::default()
        },
    )
}

/// Build the styled tree given a complete picture of interaction state. Each path slice
/// identifies the node under that interaction; matching uses CSS-spec semantics —
/// :hover and :active propagate to ancestors of the deepest engaged node, while :focus
/// only matches the focused node itself.
pub fn style_tree_with_state(
    root: &Node,
    stylesheets: &[Stylesheet],
    state: InteractionState<'_>,
) -> StyledNode {
    // The root font-size feeds rem resolution for every descendant. Compute it up front
    // by treating the root as if it lived inside the user-agent default font-size.
    let raw_root = specified_values(root, stylesheets, &[], PseudoState::default());
    let root_font_size = resolve_font_size(
        raw_root.get("font-size"),
        DEFAULT_FONT_SIZE,
        DEFAULT_FONT_SIZE,
    );
    style_tree_inner(root, stylesheets, None, root_font_size, &[], &[], state)
}

fn pseudo_state_for(state: InteractionState<'_>, current_path: &[usize]) -> PseudoState {
    let prefix_match = |target: Option<&[usize]>| -> bool {
        // Hover/active propagate up: every ancestor on the way down to the deepest
        // engaged node also enters the state. starts_with returns true when the engaged
        // path begins with `current_path`, i.e. current is an ancestor (or self).
        matches!(target, Some(p) if p.starts_with(current_path))
    };
    let exact_match = |target: Option<&[usize]>| -> bool {
        // :focus only matches the focused element itself; ancestors do not pick it up.
        target == Some(current_path)
    };
    PseudoState {
        hover: prefix_match(state.hover),
        focus: exact_match(state.focus),
        active: prefix_match(state.active),
    }
}

fn style_tree_inner<'a>(
    node: &'a Node,
    stylesheets: &[Stylesheet],
    parent_values: Option<&PropertyMap>,
    root_font_size: f32,
    ancestors: &[(&'a Node, PseudoState)],
    current_path: &[usize],
    state: InteractionState<'_>,
) -> StyledNode {
    let pseudo = pseudo_state_for(state, current_path);
    let mut specified_values = specified_values(node, stylesheets, ancestors, pseudo);

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

    // Append self to the ancestor chain children see during their selector matching,
    // carrying the resolved pseudo-state so descendant matches can check pseudo classes
    // anchored on engaged ancestors (e.g. `.outer:hover .inner`).
    let mut child_ancestors: Vec<(&Node, PseudoState)> = ancestors.to_vec();
    child_ancestors.push((node, pseudo));
    let children = node
        .children
        .iter()
        .enumerate()
        .map(|(idx, child)| {
            let mut child_path: Vec<usize> = current_path.to_vec();
            child_path.push(idx);
            style_tree_inner(
                child,
                stylesheets,
                Some(&specified_values),
                root_font_size,
                &child_ancestors,
                &child_path,
                state,
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

fn specified_values(
    node: &Node,
    stylesheets: &[Stylesheet],
    ancestors: &[(&Node, PseudoState)],
    pseudo: PseudoState,
) -> PropertyMap {
    let mut matched = Vec::new();

    // First collect every rule that matches this node together with its specificity and order.
    for (rule_order, rule) in stylesheets
        .iter()
        .flat_map(|sheet| sheet.rules.iter())
        .enumerate()
    {
        if let Some(specificity) = matching_specificity(node, pseudo, ancestors, &rule.selectors) {
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

fn matching_specificity(
    node: &Node,
    pseudo: PseudoState,
    ancestors: &[(&Node, PseudoState)],
    selectors: &[Selector],
) -> Option<u32> {
    // The highest matching selector wins within a rule group such as `h1, .title`.
    selectors
        .iter()
        .filter(|selector| matches_selector(node, pseudo, ancestors, selector))
        .map(selector_specificity)
        .max()
}

fn selector_specificity(selector: &Selector) -> u32 {
    // Sum specificity across the whole chain so descendant selectors will already give the
    // right answer once the parser starts emitting them.
    selector.parts.iter().map(simple_specificity).sum()
}

fn simple_specificity(simple: &SimpleSelector) -> u32 {
    let kind_specificity = match &simple.kind {
        SimpleSelectorKind::Tag(_) => 1,
        SimpleSelectorKind::Class(_) => 10,
        SimpleSelectorKind::Id(_) => 100,
    };
    // Each pseudo-class adds 10 (CSS spec aligns it with class specificity).
    let pseudo_specificity = if simple.pseudo.is_some() { 10 } else { 0 };
    kind_specificity + pseudo_specificity
}

fn matches_selector(
    node: &Node,
    pseudo: PseudoState,
    ancestors: &[(&Node, PseudoState)],
    selector: &Selector,
) -> bool {
    // Right-to-left matching: the rightmost simple selector is the target and must match
    // the element being styled. Each preceding part is checked against ancestors using the
    // combinator that connects it to the part on its right:
    //   - Descendant: walk up until any ancestor matches.
    //   - Child: the very next ancestor must match; no skipping.
    // Pseudo state for both the target and each ancestor is carried alongside the node so
    // pseudo-classes anchored anywhere on the chain (e.g. `.outer:hover .inner`) work.
    let Some((target, leading)) = selector.parts.split_last() else {
        return false;
    };
    if !matches_simple(node, pseudo, target) {
        return false;
    }

    let mut ancestor_iter = ancestors.iter().rev();
    for (j, part) in leading.iter().enumerate().rev() {
        let combinator = selector.combinators[j];
        match combinator {
            Combinator::Descendant => loop {
                match ancestor_iter.next() {
                    Some((ancestor, ancestor_pseudo))
                        if matches_simple(ancestor, *ancestor_pseudo, part) =>
                    {
                        break;
                    }
                    Some(_) => continue,
                    None => return false,
                }
            },
            Combinator::Child => match ancestor_iter.next() {
                Some((ancestor, ancestor_pseudo))
                    if matches_simple(ancestor, *ancestor_pseudo, part) => {}
                _ => return false,
            },
        }
    }
    true
}

fn matches_simple(node: &Node, pseudo: PseudoState, simple: &SimpleSelector) -> bool {
    let element = match &node.node_type {
        NodeType::Element(element) => element,
        // Text nodes never match selectors directly; they only inherit style from parents.
        NodeType::Text(_) => return false,
    };

    let kind_match = match &simple.kind {
        SimpleSelectorKind::Tag(tag_name) => element.tag_name == *tag_name,
        SimpleSelectorKind::Class(class_name) => has_class(element, class_name),
        SimpleSelectorKind::Id(id) => element
            .attributes
            .get("id")
            .is_some_and(|value| value == id),
    };

    if !kind_match {
        return false;
    }

    match simple.pseudo {
        None => true,
        Some(PseudoClass::Hover) => pseudo.hover,
        Some(PseudoClass::Focus) => pseudo.focus,
        Some(PseudoClass::Active) => pseudo.active,
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
    fn child_selector_matches_only_immediate_parent() {
        // .outer > .inner should NOT match when a <section> sits between the two —
        // unlike descendant, the child combinator forbids skipping.
        let nested = parse_html(
            r#"<div class="outer"><section><span class="inner">hi</span></section></div>"#,
        );
        let stylesheet = parse_css(".outer > .inner { color: #ff0000; }");
        let styled = style::style_tree(&nested, &[stylesheet]);
        let inner = &styled.children[0].children[0];

        assert_eq!(inner.value("color"), None);
    }

    #[test]
    fn child_selector_matches_when_parent_is_direct() {
        let direct = parse_html(r#"<div class="outer"><span class="inner">hi</span></div>"#);
        let stylesheet = parse_css(".outer > .inner { color: #ff0000; }");
        let styled = style::style_tree(&direct, &[stylesheet]);
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
    fn mixed_descendant_and_child_combinators_compose_correctly() {
        // `nav ul > li` requires: target is <li>, its parent is <ul>, and somewhere up
        // the chain a <nav> ancestor exists.
        let root =
            parse_html(r#"<nav class="primary"><div><ul><li class="t">hi</li></ul></div></nav>"#);
        let stylesheet = parse_css("nav ul > li { color: #ff0000; }");
        let styled = style::style_tree(&root, &[stylesheet]);
        let li = &styled.children[0].children[0].children[0];

        assert_eq!(
            li.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn hover_pseudo_class_matches_when_hovered_path_targets_node() {
        // Build: <div><a class="btn">click</a></div>. The root has one child (the <a>),
        // so the <a>'s DOM path is [0]. Telling style_tree_with_hover that [0] is hovered
        // should activate the .btn:hover rule.
        let root = parse_html(r#"<div><a class="btn">click</a></div>"#);
        let stylesheet = parse_css(
            r#"
                .btn { color: #00ff00; }
                .btn:hover { color: #ff0000; }
            "#,
        );
        let styled = style::style_tree_with_hover(&root, &[stylesheet], Some(&[0]));
        let link = &styled.children[0];

        assert_eq!(
            link.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn hover_pseudo_class_only_applies_to_the_hovered_node_not_siblings() {
        // Two .btn siblings; only the first ([0,0]) is "hovered". The second should keep
        // the non-hover color, proving the hovered_path identifies a single node.
        let root = parse_html(r#"<div><a class="btn">a</a><a class="btn">b</a></div>"#);
        let stylesheet = parse_css(
            r#"
                .btn { color: #00ff00; }
                .btn:hover { color: #ff0000; }
            "#,
        );
        let styled = style::style_tree_with_hover(&root, &[stylesheet], Some(&[0]));
        let first = &styled.children[0];
        let second = &styled.children[1];

        assert_eq!(
            first.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
        assert_eq!(
            second.value("color"),
            Some(&Value::Color(Color {
                r: 0,
                g: 255,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn hover_on_ancestor_propagates_through_descendant_combinator() {
        // .outer:hover .inner — when the .outer ancestor is hovered, the descendant
        // .inner picks up the rule even though .inner itself isn't under the cursor.
        let root = parse_html(r#"<div class="outer"><span class="inner">x</span></div>"#);
        let stylesheet = parse_css(
            r#"
                .inner { color: #00ff00; }
                .outer:hover .inner { color: #ff0000; }
            "#,
        );
        // Path [] is the root <div class="outer">.
        let styled = style::style_tree_with_hover(&root, &[stylesheet], Some(&[]));
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
    fn focus_pseudo_class_matches_only_the_focused_node_not_ancestors() {
        // <div><a class="btn">click</a></div>; mark the .btn at path [0] as focused.
        // The .btn rule must fire, but focus does NOT bubble: a hypothetical .root:focus
        // wouldn't match the outer <div>. We assert the positive case here; the negative
        // is covered by the "no engaged path" test.
        let root = parse_html(r#"<div><a class="btn">click</a></div>"#);
        let stylesheet = parse_css(
            r#"
                .btn { color: #00ff00; }
                .btn:focus { color: #ff0000; }
            "#,
        );
        let styled = style::style_tree_with_state(
            &root,
            &[stylesheet],
            style::InteractionState {
                focus: Some(&[0]),
                ..Default::default()
            },
        );
        let link = &styled.children[0];

        assert_eq!(
            link.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn focus_pseudo_class_does_not_propagate_to_ancestors() {
        // Hover the deepest text under .outer; .outer:focus should NOT match because
        // focus is anchored to the focused node alone, not its ancestor chain. We use
        // the focus path of the deeper text node and verify .outer keeps its non-focus color.
        let root = parse_html(r#"<div class="outer"><span class="inner">x</span></div>"#);
        let stylesheet = parse_css(
            r#"
                .outer { color: #00ff00; }
                .outer:focus { color: #ff0000; }
            "#,
        );
        // Focus path [0, 0] = the text node inside .inner. .outer is the root.
        let styled = style::style_tree_with_state(
            &root,
            &[stylesheet],
            style::InteractionState {
                focus: Some(&[0, 0]),
                ..Default::default()
            },
        );

        // .outer keeps the green non-focus color even though the focused node is its
        // grand-descendant. The :hover equivalent would have matched here.
        assert_eq!(
            styled.value("color"),
            Some(&Value::Color(Color {
                r: 0,
                g: 255,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn active_pseudo_class_propagates_like_hover() {
        // .active matches both the deepest active node and its ancestors, mirroring
        // :hover semantics.
        let root = parse_html(r#"<div class="outer"><span class="inner">x</span></div>"#);
        let stylesheet = parse_css(
            r#"
                .outer { color: #00ff00; }
                .outer:active { color: #ff0000; }
            "#,
        );
        // Active path [0, 0] = the text node; .outer (path []) is its ancestor.
        let styled = style::style_tree_with_state(
            &root,
            &[stylesheet],
            style::InteractionState {
                active: Some(&[0, 0]),
                ..Default::default()
            },
        );

        assert_eq!(
            styled.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn hover_propagates_from_deeply_hovered_descendant_to_ancestors() {
        // Deepest hovered node is the text inside .btn (path [0, 0]). The CSS spec says
        // every ancestor on the way down also enters :hover, so the .btn rule should
        // apply even though the cursor is over its text child.
        let root = parse_html(r#"<a class="btn">click</a>"#);
        let stylesheet = parse_css(
            r#"
                .btn { color: #00ff00; }
                .btn:hover { color: #ff0000; }
            "#,
        );
        let styled = style::style_tree_with_hover(&root, &[stylesheet], Some(&[0]));

        assert_eq!(
            styled.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn hover_pseudo_class_does_not_match_when_no_hover_path_is_given() {
        // The legacy entry point — `style_tree` without hover info — defaults to "nothing
        // is hovered", so any :hover rule should silently fail to match. A bare-class
        // fallback confirms the surrounding cascade still works.
        let root = parse_html(r#"<a class="btn">click</a>"#);
        let stylesheet = parse_css(
            r#"
                .btn { color: #00ff00; }
                .btn:hover { color: #ff0000; }
            "#,
        );
        let styled = style::style_tree(&root, &[stylesheet]);

        // The non-hover rule wins because the hover rule never matches.
        assert_eq!(
            styled.value("color"),
            Some(&Value::Color(Color {
                r: 0,
                g: 255,
                b: 0,
                a: 255,
            }))
        );
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
