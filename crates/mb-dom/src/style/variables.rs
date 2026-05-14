// CSS custom-property (`--*`) substitution. Walks a property map and
// rewrites every top-level `Value::Var` to the looked-up custom-property
// value (with cycle detection + fallback handling). Composite values
// (gradients, shadows, transforms) are intentionally not walked into —
// only top-level Var values get substituted, which is what real pages
// reach for (`color: var(--accent)`).

use std::collections::{HashMap, HashSet};

use crate::css::Value;

use super::PropertyMap;

/// Substitute every `Value::Var` in `values` with the looked-up `--*` value
/// in the same map. Custom properties inherit, so by the time this runs the
/// parent's declarations have already been folded in by the cascade caller.
///
/// Resolution is iterative: a variable that resolves to another variable is
/// chased again, with a `seen` set guarding against cycles. The fallback
/// branch fires only when the named property isn't present at all; once
/// substitution lands on something that *is* present, we use it even if the
/// caller also supplied a fallback. Composite values (gradients, shadows,
/// transforms, …) are not walked into — only top-level Var values are
/// substituted, which covers `color: var(--accent)` style use which is what
/// 5.1's site-color recovery target needs.
pub(super) fn resolve_var_references(values: &mut PropertyMap) {
    let custom_props: HashMap<String, Value> = values
        .iter()
        .filter(|(name, _)| name.starts_with("--"))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();

    for (name, value) in values.iter_mut() {
        if name.starts_with("--") {
            // Custom-property *definitions* are kept as-is so descendants
            // that inherit them still see the original (possibly Var)
            // value. Each descendant runs its own resolve pass.
            continue;
        }
        resolve_var_value(value, &custom_props);
    }
}

fn resolve_var_value(value: &mut Value, custom_props: &HashMap<String, Value>) {
    let mut seen: HashSet<String> = HashSet::new();
    while let Value::Var { name, fallback } = value {
        if !seen.insert(name.clone()) {
            // Cycle detected — collapse to the spec-defined "initial" sentinel.
            *value = Value::Keyword("initial".into());
            return;
        }
        match custom_props.get(name) {
            Some(resolved) => {
                *value = resolved.clone();
            }
            None => {
                let fb = fallback.take();
                *value = match fb {
                    Some(boxed) => *boxed,
                    None => Value::Keyword("initial".into()),
                };
            }
        }
    }
}
