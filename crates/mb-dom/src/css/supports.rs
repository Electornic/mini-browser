//! `@supports` conditions (compat round 3 step 2.3).
//!
//! The condition is evaluated for real, once, when the sheet is parsed —
//! nothing in it depends on the viewport. A `@supports` block whose
//! condition is false is parsed (its layers still take their place, the
//! survey report still sees it) and its rules are dropped.
//!
//! "Supported" means **rendered**, not merely parsed: `@supports` usually
//! guards a modern feature with a fallback in the `not (…)` branch, and the
//! fallback is closer to the real page when we can't draw the feature. So
//! `(prop: value)` is true only when the property is one we model (a typed
//! or dedicated [`ValueKind`] — never [`ValueKind::Other`]), the value
//! parses into that property's type (no `Keyword` fallback, no `var()`),
//! every longhand it sets is one the engine reads
//! ([`properties::RENDERED`]), and the pair isn't on the [`UNRENDERED`]
//! list (values of read properties we still don't draw). Custom properties are
//! the exception: `(--x: 0)` is true, as in every browser. `selector(…)`
//! parses its argument with the same, non-forgiving selector parser the
//! cascade uses. Anything else in parentheses or a function is the spec's
//! `<general-enclosed>`: false.
//!
//! [`evaluate`] is the public entry point a future `CSS.supports()` will
//! share.

use cssparser::{Parser as CssParser, Token};

use super::properties::{self, ValueKind};
use super::{Value, media::MAX_MEDIA_NESTING};

/// Values (or whole properties, `None`) that parse but the engine doesn't
/// render, so a condition naming them is false. A name ending in `*` is a
/// prefix. When a feature here gets implemented, remove its entry in the
/// same change (`supports_unrendered_list_matches_layout` in mb-engine
/// pins the value entries).
pub const UNRENDERED: &[(&str, Option<&str>)] = &[
    // Layout treats these as static / block.
    ("position", Some("sticky")),
    ("position", Some("-webkit-sticky")),
    ("display", Some("contents")),
    ("display", Some("-webkit-box")),
    ("display", Some("-webkit-inline-box")),
    ("display", Some("ruby")),
    ("display", Some("ruby-text")),
    ("display", Some("math")),
    // Whole properties nothing reads (all `ValueKind::Other` today, so
    // already false — listed so a later typed entry doesn't flip them).
    ("aspect-ratio", None),
    ("backdrop-filter", None),
    ("mask*", None),
    ("container*", None),
    ("content-visibility", None),
    ("field-sizing", None),
    ("interpolate-size", None),
    ("anchor-name", None),
    ("position-anchor", None),
    ("position-area", None),
];

/// A parsed `@supports` condition.
#[derive(Debug, Clone, PartialEq)]
pub enum SupportsCondition {
    Not(Box<SupportsCondition>),
    And(Vec<SupportsCondition>),
    Or(Vec<SupportsCondition>),
    /// `(name: value)`, the name lower-cased, the value as written.
    Declaration {
        name: String,
        value: String,
    },
    /// `selector(…)`, the argument as written.
    Selector(String),
    /// `<general-enclosed>` — any other parenthesized text or function
    /// (`font-tech(…)`, `(frobnicate)`), and anything nested past the cap.
    Unknown,
}

impl SupportsCondition {
    /// The whole prelude of `@supports`; `None` when it isn't a valid
    /// condition (the rule is dropped, as in Chrome).
    pub fn parse(prelude: &str) -> Option<Self> {
        let mut parser = CssParser::new(prelude);
        Self::parse_from(&mut parser)
    }

    /// Parse a condition that fills `input` to its end.
    pub(super) fn parse_from(input: &mut CssParser<'_>) -> Option<Self> {
        input
            .parse_entirely(|input| {
                condition(input, 0).ok_or_else(|| cssparser::ParseError::<()>::custom(()))
            })
            .ok()
    }
}

/// Whether the engine supports `condition` (see the module docs).
pub fn evaluate(condition: &SupportsCondition) -> bool {
    match condition {
        SupportsCondition::Not(inner) => !evaluate(inner),
        SupportsCondition::And(all) => all.iter().all(evaluate),
        SupportsCondition::Or(any) => any.iter().any(evaluate),
        SupportsCondition::Declaration { name, value } => declaration_supported(name, value),
        SupportsCondition::Selector(selector) => selector_supported(selector),
        SupportsCondition::Unknown => false,
    }
}

/// `<supports-condition>`: `not X` | `X [and X]*` | `X [or X]*`.
/// `depth` counts enclosing parentheses; recursion stops at the cap.
fn condition(input: &mut CssParser<'_>, depth: usize) -> Option<SupportsCondition> {
    if input
        .try_parse(|input| input.expect_ident_matching("not"))
        .is_ok()
    {
        return Some(SupportsCondition::Not(Box::new(in_parens(input, depth)?)));
    }
    let first = in_parens(input, depth)?;
    let mut terms = vec![first];
    let mut joiner: Option<bool> = None; // Some(true) = and, Some(false) = or
    loop {
        let word = match input.try_parse(|input| input.expect_ident_cloned()) {
            Ok(word) => word.to_ascii_lowercase(),
            Err(_) => break,
        };
        let is_and = match word.as_str() {
            "and" => true,
            "or" => false,
            _ => return None,
        };
        // `a and b or c` needs parentheses.
        if joiner.is_some_and(|j| j != is_and) {
            return None;
        }
        joiner = Some(is_and);
        terms.push(in_parens(input, depth)?);
    }
    Some(match joiner {
        None => terms.pop()?,
        Some(true) => SupportsCondition::And(terms),
        Some(false) => SupportsCondition::Or(terms),
    })
}

/// `<supports-in-parens>`: `( condition )` | `( declaration )` |
/// `selector( … )` | `<general-enclosed>`.
fn in_parens(input: &mut CssParser<'_>, depth: usize) -> Option<SupportsCondition> {
    match input.next().ok()?.clone() {
        Token::ParenthesisBlock => {
            if depth >= MAX_MEDIA_NESTING {
                // Too deep to look inside: cssparser skips the block
                // without recursing.
                return Some(SupportsCondition::Unknown);
            }
            input
                .parse_nested_block(|input| {
                    let state = input.state();
                    if let Ok(nested) = input.try_parse(|input| {
                        condition(input, depth + 1)
                            .ok_or_else(|| cssparser::ParseError::<()>::custom(()))
                    }) && input.is_exhausted()
                    {
                        return Ok(nested);
                    }
                    input.reset(&state);
                    if let Some(declaration) = declaration(input) {
                        return Ok(declaration);
                    }
                    // `<general-enclosed>`: consume the rest (the block
                    // must be used up — `parse_nested_block` is
                    // `parse_entirely`).
                    input.reset(&state);
                    while input.next_including_whitespace_and_comments().is_ok() {}
                    Ok::<_, cssparser::ParseError<()>>(SupportsCondition::Unknown)
                })
                .ok()
        }
        Token::Function(name) => {
            let is_selector = name.eq_ignore_ascii_case("selector");
            input
                .parse_nested_block(|input| {
                    let start = input.position();
                    while input.next_including_whitespace_and_comments().is_ok() {}
                    let text = input.slice_from(start).trim().to_string();
                    Ok::<_, cssparser::ParseError<()>>(if is_selector {
                        SupportsCondition::Selector(text)
                    } else {
                        SupportsCondition::Unknown
                    })
                })
                .ok()
        }
        _ => None,
    }
}

/// `name: value` filling the block, or `None`.
fn declaration(input: &mut CssParser<'_>) -> Option<SupportsCondition> {
    let name = input.expect_ident_cloned().ok()?.to_ascii_lowercase();
    input.expect_colon().ok()?;
    let start = input.position();
    while input.next_including_whitespace_and_comments().is_ok() {}
    let value = input.slice_from(start).trim().to_string();
    Some(SupportsCondition::Declaration { name, value })
}

fn declaration_supported(name: &str, value: &str) -> bool {
    // Custom properties take any token sequence, in every browser.
    if name.starts_with("--") {
        return true;
    }
    // A `-webkit-` alias Chrome accepts is judged as the standard name.
    let name = properties::unprefixed(name);
    let Some(kind) = properties::lookup(name) else {
        return false;
    };
    // `Other`: a real property whose value we don't model — nothing to
    // render it with. `var()` can only be judged after substitution
    // (true in browsers, but rare in conditions — kept conservative).
    if kind == ValueKind::Other || value.to_ascii_lowercase().contains("var(") {
        return false;
    }
    if is_unrendered(name, value) {
        return false;
    }
    let Some(declarations) = super::parse_property_value(name, value) else {
        return false;
    };
    // Every longhand it sets must be one something downstream reads.
    !declarations.is_empty()
        && declarations.iter().all(|declaration| {
            value_fits(kind, &declaration.value) && properties::is_rendered(&declaration.name)
        })
}

fn is_unrendered(name: &str, value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    UNRENDERED.iter().any(|(pattern, unrendered)| {
        let name_matches = match pattern.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => name == *pattern,
        };
        name_matches && unrendered.is_none_or(|v| v == value)
    })
}

/// Whether a parsed value is a meaningful value of `kind` — not the
/// generic parser's `Keyword` fallback, not unparsed text.
fn value_fits(kind: ValueKind, value: &Value) -> bool {
    match value {
        Value::Unparsed { .. } | Value::Var { .. } => false,
        Value::Keyword(keyword) => kind == ValueKind::Dedicated || kind.accepts_keyword(keyword),
        Value::Length(..) => matches!(kind, ValueKind::Length(_) | ValueKind::Dedicated),
        Value::Number(_) => matches!(kind, ValueKind::Number(_) | ValueKind::Dedicated),
        Value::Color(_) => matches!(kind, ValueKind::Color(_) | ValueKind::Dedicated),
        Value::ImageUrl(_) | Value::Gradient(_) => {
            matches!(kind, ValueKind::Image(_) | ValueKind::Dedicated)
        }
        _ => kind == ValueKind::Dedicated,
    }
}

fn selector_supported(selector: &str) -> bool {
    // One complex selector, parsed without forgiveness: an unsupported
    // pseudo-class anywhere (`:is(:foo)`) makes it false, as in Chrome.
    !selector.is_empty()
        && super::parse_selector(selector)
            .is_ok_and(|list| list.list().slice().len() == 1 && !list.has_forgiven_argument())
}

#[cfg(test)]
mod tests {
    use super::{SupportsCondition, UNRENDERED, evaluate};

    fn supports(prelude: &str) -> Option<bool> {
        SupportsCondition::parse(prelude).map(|c| evaluate(&c))
    }

    #[test]
    fn supports_custom_property_true() {
        assert_eq!(supports("(--x: 0)"), Some(true));
        assert_eq!(supports("(--brand-color: rgb(1 2 3) , foo)"), Some(true));
        assert_eq!(supports("not (--x: 0)"), Some(false));
    }

    #[test]
    fn supports_typed_values() {
        assert_eq!(supports("(display: grid)"), Some(true));
        assert_eq!(supports("(display: flex) and (gap: 1px)"), Some(true));
        assert_eq!(supports("(color: light-dark(red, red))"), Some(true));
        assert_eq!(supports("(width: inherit)"), Some(true));
        assert_eq!(supports("(DISPLAY: GRID)"), Some(true));
        // A dedicated parser's result counts.
        assert_eq!(supports("(border: 1px solid red)"), Some(true));
        assert_eq!(supports("(display: flexbox)"), Some(false));
        // Every named colour (mdn's `not (color: light-dark(tan, tan))`).
        assert_eq!(supports("(color: light-dark(tan, tan))"), Some(true));
        assert_eq!(supports("not (color: light-dark(tan, tan))"), Some(false));
        // Typed, parsed, but nothing reads it: unsupported.
        assert_eq!(supports("(overflow-wrap: anywhere)"), Some(false));
        assert_eq!(supports("(word-break: break-word)"), Some(false));
        assert_eq!(supports("(visibility: hidden)"), Some(false));
    }

    #[test]
    fn supports_unknown_property_false() {
        assert_eq!(supports("(colour: red)"), Some(false));
        assert_eq!(supports("(-moz-appearance: none)"), Some(false));
        assert_eq!(supports("not (colour: red)"), Some(true));
    }

    #[test]
    fn supports_keyword_fallback_value_false() {
        // `100vw` is kept by the generic parser as a `Keyword` the length
        // property doesn't accept — not a value we render (step 5 later).
        assert_eq!(supports("(width: 100vw)"), Some(false));
        assert_eq!(supports("(height: 100dvh)"), Some(false));
        assert_eq!(supports("(width: round(1.5px, 1px))"), Some(false));
        assert_eq!(supports("(width: 5)"), Some(false));
        assert_eq!(supports("(color: 12px)"), Some(false));
        // `var()` is judged false (conservative), unlike `--x` itself.
        assert_eq!(supports("(color: var(--x))"), Some(false));
    }

    #[test]
    fn supports_unrendered_value_false() {
        assert_eq!(supports("(position: sticky)"), Some(false));
        assert_eq!(supports("(position: -webkit-sticky)"), Some(false));
        assert_eq!(supports("(display: contents)"), Some(false));
        assert_eq!(supports("(position: relative)"), Some(true));
        // Properties we don't model at all (`Other`).
        for condition in [
            "(backdrop-filter: initial)",
            "(-webkit-backdrop-filter: blur(1px))",
            "(mask-image: none)",
            "(-webkit-mask-image: none) or (mask-image: none)",
            "(container-type: inline-size)",
            "(aspect-ratio: 1 / 1)",
            "(content-visibility: hidden)",
            "(-webkit-touch-callout: none)",
        ] {
            assert_eq!(supports(condition), Some(false), "{condition}");
        }
        assert!(UNRENDERED.iter().all(|(name, _)| !name.is_empty()));
    }

    #[test]
    fn webkit_aliases_are_judged_as_the_standard_name() {
        assert_eq!(
            supports("(-webkit-transform: translate(1px))"),
            supports("(transform: translate(1px))")
        );
        assert_eq!(supports("(-webkit-transform: translate(1px))"), Some(true));
        assert_eq!(
            supports("(-webkit-backdrop-filter: none)"),
            supports("(backdrop-filter: none)")
        );
    }

    #[test]
    fn supports_logic() {
        assert_eq!(supports("not (display: frob)"), Some(true));
        assert_eq!(supports("(display: grid) and (display: frob)"), Some(false));
        assert_eq!(supports("(display: frob) or (display: grid)"), Some(true));
        assert_eq!(
            supports("((display: grid) and (color: red)) or (frob: 1)"),
            Some(true)
        );
        assert_eq!(supports("not ((display: grid) or (frob: 1))"), Some(false));
        // `<general-enclosed>` is false; `not` of it is true.
        assert_eq!(supports("(frobnicate)"), Some(false));
        assert_eq!(supports("font-tech(color-COLRv1)"), Some(false));
        assert_eq!(supports("not (frob bar baz)"), Some(true));
    }

    #[test]
    fn invalid_conditions_are_none() {
        for prelude in [
            "",
            "display: grid",
            "(display: grid) and (color: red) or (width: 1px)",
            "(display: grid) (color: red)",
            "not not (display: grid)",
            "(display: grid) and",
            "(display: grid) xor (color: red)",
            "and (display: grid)",
        ] {
            assert_eq!(supports(prelude), None, "{prelude:?}");
        }
    }

    #[test]
    fn supports_selector_function() {
        assert_eq!(supports("selector(div > p)"), Some(true));
        assert_eq!(supports("selector(a:hover)"), Some(true));
        assert_eq!(supports("selector(:frobnicate)"), Some(false));
        assert_eq!(supports("selector(a, b)"), Some(false));
        assert_eq!(supports("selector()"), Some(false));
        assert_eq!(supports("not selector(:frobnicate)"), Some(true));
    }

    #[test]
    fn supports_selector_is_not_forgiving() {
        assert_eq!(supports("selector(:is(:foo))"), Some(false));
        assert_eq!(supports("selector(:is(a, :foo))"), Some(false));
    }

    #[test]
    fn deep_conditions_are_bounded() {
        // Past the cap a parenthesized condition is `<general-enclosed>`
        // (false); 10,000 levels neither overflow the stack nor hang.
        let depth = 10_000;
        let deep = format!("{}display: grid{}", "(".repeat(depth), ")".repeat(depth));
        assert_eq!(supports(&deep), Some(false));
        assert_eq!(supports(&format!("not {deep}")), Some(true));
        let ok = format!("{}display: grid{}", "(".repeat(20), ")".repeat(20));
        assert_eq!(supports(&ok), Some(true));
        let chain = vec!["(display: grid)"; 50_000].join(" and ");
        assert_eq!(supports(&chain), Some(true));
        let nots = "not (".repeat(depth) + "display: grid" + &")".repeat(depth);
        assert!(supports(&nots).is_some());
    }
}
