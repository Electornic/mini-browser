// CSS custom-property (`--*`) substitution. Walks a property map and
// rewrites every top-level `Value::Var` to the looked-up custom-property
// value (with cycle detection + fallback handling). Composite values
// (gradients, shadows, transforms) are intentionally not walked into —
// only top-level Var values get substituted, which is what real pages
// reach for (`color: var(--accent)`).

use std::{cell::RefCell, collections::HashSet, rc::Rc};

use crate::css::{Value, properties};

use super::{PropertyMap, StyleReport, declaration_key};

/// The custom properties (`--*`) in scope at a node: its own declarations
/// on top of the parent's scope. Custom properties inherit, so a node that
/// declares none shares its parent's scope (one `Rc` clone) — pages put
/// hundreds of design tokens on `:root`, and copying them into every node
/// made style time grow with nodes × tokens (compat round 3 step 1r).
/// Lookups walk up the chain; it only grows at nodes that declare `--*`
/// and is bounded by the DOM depth cap.
#[derive(Debug, Default)]
pub(super) struct CustomScope {
    vars: PropertyMap,
    parent: Option<Rc<CustomScope>>,
}

impl CustomScope {
    /// The scope of a node declaring `own` under `parent`.
    pub(super) fn child(parent: &Rc<CustomScope>, own: PropertyMap) -> Rc<CustomScope> {
        if own.is_empty() {
            Rc::clone(parent)
        } else {
            Rc::new(CustomScope {
                vars: own,
                parent: Some(Rc::clone(parent)),
            })
        }
    }

    fn get(&self, name: &str) -> Option<&Value> {
        let mut scope = self;
        loop {
            if let Some(value) = scope.vars.get(name) {
                return Some(value);
            }
            scope = scope.parent.as_deref()?;
        }
    }
}

/// Substitute every `Value::Var` in `values` (a node's non-custom
/// properties) with the looked-up `--*` value in `scope`.
///
/// Resolution is iterative: a variable that resolves to another variable is
/// chased again, with a `seen` set guarding against cycles. The fallback
/// branch fires only when the named property isn't present at all; once
/// substitution lands on something that *is* present, we use it even if the
/// caller also supplied a fallback. Composite values (gradients, shadows,
/// transforms, …) are not walked into — only top-level Var values are
/// substituted, which covers `color: var(--accent)` style use which is what
/// 5.1's site-color recovery target needs.
/// `report` (the compat probe's pass only) collects the declarations whose
/// substitution failed or left a keyword the property rejects.
pub(super) fn resolve_var_references(
    values: &mut PropertyMap,
    scope: &CustomScope,
    report: Option<&RefCell<StyleReport>>,
) {
    for (name, value) in values.iter_mut() {
        let Some(report) = report else {
            if resolve_var_value(value, scope) {
                promote_unitless_zero(name, value);
            }
            continue;
        };
        if !matches!(value, Value::Var { .. }) {
            continue;
        }
        let declared = value.clone();
        let resolved = resolve_var_value(value, scope);
        if resolved {
            promote_unitless_zero(name, value);
        }
        let mut report = report.borrow_mut();
        if !resolved {
            report.iacvt.insert(declaration_key(name, &declared));
        } else if let (Some(kind), Value::Keyword(keyword)) = (properties::lookup(name), &*value)
            && kind.is_typed()
            && !kind.accepts_keyword(keyword)
        {
            report
                .var_keyword_fallback
                .insert(declaration_key(name, &declared));
        }
    }
}

/// A substituted bare `0` in a length property is `0px`, as at parse time
/// (`css::unitless_zero_as_length`, step 1q).
fn promote_unitless_zero(name: &str, value: &mut Value) {
    if matches!(value, Value::Number(n) if *n == 0.0) {
        *value = crate::css::unitless_zero_as_length(name, value.clone());
    }
}

/// Substitute in place. False = invalid at computed-value time (a cycle,
/// or an undefined name with no fallback): the value became `initial`.
fn resolve_var_value(value: &mut Value, scope: &CustomScope) -> bool {
    let mut seen: HashSet<String> = HashSet::new();
    while let Value::Var { name, fallback } = value {
        if !seen.insert(name.clone()) {
            // Cycle detected — collapse to the spec-defined "initial" sentinel.
            *value = Value::Keyword("initial".into());
            return false;
        }
        match scope.get(name) {
            Some(resolved) => {
                *value = resolved.clone();
            }
            None => {
                let fb = fallback.take();
                match fb {
                    Some(boxed) => *value = *boxed,
                    None => {
                        *value = Value::Keyword("initial".into());
                        return false;
                    }
                }
            }
        }
    }
    true
}
