// CSS cascade for a single node: walk all stylesheets, collect every rule
// whose selector matches, fold UA defaults + presentational hints + author
// rules + inline `style="…"` together by specificity + source order, and
// emit the resulting `PropertyMap`. The `<center>` quirks-mode auto-margin
// shim folds in last because real browsers run it regardless of author CSS.

use std::cmp::Reverse;

use selectors::context::{
    MatchingContext, MatchingForInvalidation, MatchingMode, NeedsSelectorFlags, QuirksMode,
    SelectorCaches,
};

use crate::{
    css::{Declaration, MiniBrowserSelectorImpl, Rule, Value, layer::UNLAYERED},
    dom::{Document, NodeId, NodeType},
    dom_select::{MatchingElement, MatchingState},
};

use super::{Cascade, PropertyMap, defaults::default_values, presentational::presentational_hints};

pub(super) fn specified_values(
    document: &Document,
    node_id: NodeId,
    cascade: &Cascade<'_>,
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
    // layer rank, specificity and source order. Only the selectors the
    // sheet's rule index files under this node's id / classes / tag (plus
    // the universal ones) can match — the rest are never tried (F4). Rules
    // come out in source order, as the old all-rules walk produced them.
    // A rule in a `@media` block that doesn't match is skipped; its slot
    // in `rule_order` stays, so source order is unchanged for the rest.
    let (id, class, tag) = match document.get(node_id).map(|n| &n.node_type) {
        Some(NodeType::Element(e)) => (
            e.attributes.get("id").map(String::as_str),
            e.attributes.get("class").map(String::as_str),
            Some(e.tag_name.as_str()),
        ),
        _ => (None, None, None),
    };
    let no_ranks = Vec::new();
    let no_media = Vec::new();
    let mut candidates = Vec::new();
    let mut sheet_offset = 0usize;
    for (sheet_i, sheet) in cascade.stylesheets.iter().enumerate() {
        let media = cascade.media.get(sheet_i).unwrap_or(&no_media);
        let ranks = cascade.layer_ranks.get(sheet_i).unwrap_or(&no_ranks);
        candidates.clear();
        sheet.index.candidates(id, class, tag, &mut candidates);
        candidates.sort_unstable();
        candidates.dedup();
        let mut at = 0;
        while let Some(&(rule_i, _)) = candidates.get(at) {
            // This rule's candidate selectors are consecutive.
            let end = candidates[at..]
                .iter()
                .position(|&(r, _)| r != rule_i)
                .map_or(candidates.len(), |n| at + n);
            let selectors = &candidates[at..end];
            at = end;
            let Some(rule) = sheet.rules.get(rule_i as usize) else {
                continue;
            };
            let applies = rule
                .media
                .as_ref()
                .is_none_or(|block| media.get(block.index).copied().unwrap_or(false));
            if !applies {
                continue;
            }
            let list = rule.selectors.list().slice();
            let specificity = selectors
                .iter()
                .filter_map(|&(_, selector_i)| list.get(selector_i as usize))
                .filter(|selector| {
                    selectors::matching::matches_selector(selector, 0, None, element, &mut ctx)
                })
                .map(|selector| selector.specificity())
                .max();
            if let Some(specificity) = specificity {
                let rank = rule
                    .layer
                    .and_then(|layer| ranks.get(layer as usize).copied())
                    .unwrap_or(UNLAYERED);
                matched.push(Matched {
                    rank,
                    specificity,
                    rule_order: sheet_offset + rule_i as usize,
                    rule,
                });
            }
        }
        sheet_offset += sheet.rules.len();
    }

    // Lower-priority rules are applied first so later, more specific
    // matches overwrite them. Normal declarations: an earlier layer loses
    // to a later one and every layer loses to unlayered rules (step 2.2).
    matched.sort_by_key(|m| (m.rank, m.specificity, m.rule_order));
    // `!important` reverses the layer order (CSS Cascade 5 §6.4): unlayered
    // important loses to every layer's, an earlier layer's beats a later's.
    let mut important: Vec<&Matched<'_>> =
        matched.iter().filter(|m| m.rule.has_important).collect();
    important.sort_by_key(|m| (Reverse(m.rank), m.specificity, m.rule_order));

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
    // Inline `style="..."` carries the highest author specificity per the CSS
    // spec — it beats every selector match, even an ID selector. Applied
    // after the matched rules so a `<td style="width: 60px">` value lands on
    // top of any author `td { width: 100px; }` rule.
    let inline_decls = match document.get(node_id).map(|n| &n.node_type) {
        Some(NodeType::Element(elem_data)) => elem_data
            .attributes
            .get("style")
            .map(|source| crate::css::parse_inline_style(source))
            .unwrap_or_default(),
        _ => Vec::new(),
    };

    // Two passes in the spec's origin order (CSS Cascade 4 §6.1; no UA
    // `!important` or user sheets here): author normal < inline normal <
    // author important < inline important. Within a pass, the
    // (layer, specificity, source order) sort decides — the style
    // attribute is compared before layers, so it beats every layer in its
    // pass. Rules without an `!important` declaration skip the second
    // pass (`Rule::has_important`).
    for m in &matched {
        apply_declarations(&mut values, &m.rule.declarations, false);
    }
    apply_declarations(&mut values, &inline_decls, false);
    for m in important {
        apply_declarations(&mut values, &m.rule.declarations, true);
    }
    apply_declarations(&mut values, &inline_decls, true);

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

/// A rule matching the node, with what the cascade sorts it by.
struct Matched<'a> {
    /// Document-wide layer rank ([`UNLAYERED`] outside every layer).
    rank: u32,
    specificity: u32,
    rule_order: usize,
    rule: &'a Rule,
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

/// Fold the declarations of one importance into `values`; later ones with
/// the same property name overwrite earlier ones.
fn apply_declarations(values: &mut PropertyMap, declarations: &[Declaration], important: bool) {
    for declaration in declarations.iter().filter(|d| d.important == important) {
        if declaration.name == "all" {
            apply_all(values, &declaration.value);
            continue;
        }
        values.insert(declaration.name.clone(), declaration.value.clone());
    }
}

/// `all: <css-wide keyword>` (step 1b): every property set so far — UA
/// defaults and lower-priority declarations included — and every inherited
/// one takes the keyword; custom properties are exempt (CSS Cascade 4
/// §3.2). Declarations after it in the cascade still win. The keyword is
/// resolved per property afterwards (`resolve_css_wide_keywords`).
fn apply_all(values: &mut PropertyMap, keyword: &Value) {
    for (name, value) in values.iter_mut() {
        if !name.starts_with("--") {
            *value = keyword.clone();
        }
    }
    for &name in super::INHERITED_PROPERTIES {
        values
            .entry(name.to_string())
            .or_insert_with(|| keyword.clone());
    }
}
