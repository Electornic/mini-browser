// CSS cascade for a single node: walk all stylesheets, collect every rule
// whose selector matches, fold UA defaults + presentational hints + author
// rules + inline `style="…"` together by specificity + source order, and
// emit the resulting `PropertyMap`. The `<center>` quirks-mode auto-margin
// shim folds in last because real browsers run it regardless of author CSS.

use selectors::context::{
    MatchingContext, MatchingForInvalidation, MatchingMode, NeedsSelectorFlags, QuirksMode,
    SelectorCaches,
};

use crate::{
    css::{Declaration, MiniBrowserSelectorImpl, Selector, Stylesheet, Value},
    dom::{Document, NodeId, NodeType},
    dom_select::{MatchingElement, MatchingState},
};

use super::{PropertyMap, defaults::default_values, presentational::presentational_hints};

pub(super) fn specified_values(
    document: &Document,
    node_id: NodeId,
    stylesheets: &[Stylesheet],
    state: &MatchingState,
) -> PropertyMap {
    let mut matched = Vec::new();
    let element = MatchingElement::new(node_id, document, state);

    // The selectors crate's `MatchingContext` carries caches that get
    // mutated during a match (the nth-index cache, the relative-selector
    // cache, etc.). We rebuild it once per node — the caches grow over
    // the course of one styling pass which is exactly the reuse window
    // the design intends; recreating per-rule would erase that benefit.
    let mut caches = SelectorCaches::default();
    let mut ctx = MatchingContext::<MiniBrowserSelectorImpl>::new(
        MatchingMode::Normal,
        None,
        &mut caches,
        QuirksMode::NoQuirks,
        NeedsSelectorFlags::No,
        MatchingForInvalidation::No,
    );

    // First collect every rule that matches this node together with its
    // specificity and source order.
    for (rule_order, rule) in stylesheets
        .iter()
        .flat_map(|sheet| sheet.rules.iter())
        .enumerate()
    {
        if let Some(specificity) = matching_specificity(&element, &mut ctx, &rule.selectors) {
            matched.push((specificity, rule_order, &rule.declarations));
        }
    }

    // Lower-priority rules are applied first so later, more specific matches overwrite them.
    matched.sort_by_key(|(specificity, rule_order, _)| (*specificity, *rule_order));

    let mut values = default_values(document, node_id);
    // Presentational hints fold in between UA defaults and any author rules —
    // the HTML spec calls them "presentational hints" and gives them less
    // weight than every selector match, so a `<table border="1">` value
    // surrenders to `table { border: none; }` in author CSS but still wins
    // over an unstyled UA fallback.
    if let Some(NodeType::Element(elem_data)) = document.get(node_id).map(|n| &n.node_type) {
        for (name, value) in presentational_hints(elem_data) {
            values.insert(name, value);
        }
    }
    for (_, _, declarations) in matched {
        apply_declarations(&mut values, declarations);
    }

    // Inline `style="..."` carries the highest author specificity per the CSS
    // spec — it beats every selector match, even an `!important`-free ID
    // selector. Apply after the matched-rules sort so a `<td style="width:
    // 60px">` value lands on top of any author `td { width: 100px; }` rule.
    // !important inside the inline style is not yet honoured (we don't model
    // !important anywhere); revisit once important folding is implemented.
    if let Some(NodeType::Element(elem_data)) = document.get(node_id).map(|n| &n.node_type)
        && let Some(inline_source) = elem_data.attributes.get("style")
    {
        let inline_decls = crate::css::parse_inline_style(inline_source);
        apply_declarations(&mut values, &inline_decls);
    }

    // Legacy `<center>` element: real browsers center every block child
    // through quirks-mode magic (effectively `margin: 0 auto`). HN still
    // wraps its main table in `<center>` for that reason. Without this
    // shim the table renders left-aligned, leaving a wide empty band on
    // the right. We only fill in auto-margins when the cascade hasn't
    // already supplied a horizontal margin — author CSS / presentational
    // hints stay authoritative if they declared one.
    if parent_is_center(document, node_id)
        && !values.contains_key("margin-left")
        && !values.contains_key("margin-right")
    {
        values.insert("margin-left".into(), Value::Keyword("auto".into()));
        values.insert("margin-right".into(), Value::Keyword("auto".into()));
    }

    values
}

fn parent_is_center(document: &Document, node_id: NodeId) -> bool {
    let Some(parent_id) = document.get(node_id).and_then(|n| n.parent) else {
        return false;
    };
    matches!(
        document.get(parent_id).map(|n| &n.node_type),
        Some(NodeType::Element(element)) if element.tag_name.eq_ignore_ascii_case("center")
    )
}

fn apply_declarations(values: &mut PropertyMap, declarations: &[Declaration]) {
    // Later declarations with the same property name overwrite earlier ones.
    for declaration in declarations {
        values.insert(declaration.name.clone(), declaration.value.clone());
    }
}

fn matching_specificity(
    element: &MatchingElement<'_>,
    ctx: &mut MatchingContext<'_, MiniBrowserSelectorImpl>,
    selectors: &Selector,
) -> Option<u32> {
    // The selectors crate hands us the parsed `SelectorList` (one entry
    // per `Rule`'s comma-separated selector list). `matches_selector_list`
    // returns true if any branch matches; for the cascade we also want
    // the *highest* specificity among the matching branches, so we walk
    // the list ourselves with `matches_selector` instead.
    let mut best: Option<u32> = None;
    for selector in selectors.list().slice() {
        if selectors::matching::matches_selector(selector, 0, None, element, ctx) {
            let spec = selector.specificity();
            best = Some(best.map_or(spec, |prev| prev.max(spec)));
        }
    }
    best
}
