// CSS custom properties (`--*`) and `var()` substitution (compat round 3
// step 1c). Custom properties and every value containing `var()` arrive
// from the cascade as `Value::Unparsed` source text:
//
//   1. A node's own custom properties are substituted where they are
//      declared and the result is what descendants inherit (the spec's
//      computed value) — `CustomScope`, an `Rc` chain that only grows at
//      nodes declaring `--*` (step 1r).
//   2. Every other `Unparsed` value has its `var()`s replaced with text from
//      the scope, then is re-parsed with its property's parser — or its
//      shorthand's, taking its own longhand from the result — through a
//      per-style-pass memo keyed by (property or shorthand, text).
//   3. Failure at either step is "invalid at computed-value time": the
//      property becomes `unset` (inherited → the parent's value, otherwise
//      initial), never an earlier declaration from the cascade.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap},
    rc::Rc,
};

use cssparser::{Parser as CssParser, SourcePosition, Token};

use crate::css::{Declaration, MAX_VAR_NESTING, Value, properties};

use super::{PropertyMap, StyleReport, declaration_key};

/// The longest text a substitution may produce. `--a1: var(--a0) var(--a0)`
/// chains double it per level; past this the value is invalid (Chrome caps
/// substitution too). Robustness rule 3: bounded time and memory.
pub(super) const MAX_SUBSTITUTED_LEN: usize = 64 * 1024;

/// The custom properties in scope at a node: its own (substituted) values
/// over the parent's scope. `None` is the guaranteed-invalid value — a
/// cycle, a failed substitution, or `initial` — which shadows an ancestor's
/// definition. A node that declares none shares its parent's scope.
#[derive(Debug, Default)]
pub(super) struct CustomScope {
    vars: BTreeMap<String, Option<Rc<str>>>,
    parent: Option<Rc<CustomScope>>,
}

impl CustomScope {
    /// The scope of a node declaring `own` (`--*` → `Value::Unparsed`)
    /// under `parent`: each is substituted here, references to the node's
    /// other custom properties resolved first (cycles → invalid).
    pub(super) fn child(parent: &Rc<CustomScope>, own: PropertyMap) -> Rc<CustomScope> {
        let mut raws: BTreeMap<String, Option<String>> = BTreeMap::new();
        for (name, value) in own {
            let Value::Unparsed { raw, .. } = value else {
                continue;
            };
            match raw.to_ascii_lowercase().as_str() {
                // Custom properties inherit: these keep the parent's value.
                "inherit" | "unset" | "revert" | "revert-layer" => {}
                "initial" => {
                    raws.insert(name, None);
                }
                _ => {
                    raws.insert(name, Some(raw));
                }
            }
        }
        if raws.is_empty() {
            return Rc::clone(parent);
        }
        let mut resolved: BTreeMap<String, Option<Rc<str>>> = BTreeMap::new();
        let names: Vec<String> = raws.keys().cloned().collect();
        let too_deep = Cell::new(false);
        for name in names {
            let mut in_progress = Vec::new();
            resolve_own(
                &name,
                &raws,
                parent,
                &mut resolved,
                &mut in_progress,
                0,
                &too_deep,
            );
        }
        Rc::new(CustomScope {
            vars: resolved,
            parent: Some(Rc::clone(parent)),
        })
    }

    /// The text `var(--name)` substitutes, or `None` (guaranteed-invalid
    /// or undefined anywhere: the fallback applies, else it fails).
    fn get(&self, name: &str) -> Option<&str> {
        let mut scope = self;
        loop {
            if let Some(value) = scope.vars.get(name) {
                return value.as_deref();
            }
            scope = scope.parent.as_deref()?;
        }
    }
}

/// Substitute one of a node's own custom properties, recursing into the
/// node's others it references. A name met again while in progress is a
/// cycle: guaranteed-invalid. Each link counts towards
/// [`MAX_VAR_NESTING`] together with block and fallback nesting; a result
/// that failed only because of how deep *this* walk started is not cached —
/// the same name resolved from the top may fit (order-independent).
fn resolve_own(
    name: &str,
    raws: &BTreeMap<String, Option<String>>,
    parent: &CustomScope,
    resolved: &mut BTreeMap<String, Option<Rc<str>>>,
    in_progress: &mut Vec<String>,
    depth: usize,
    too_deep: &Cell<bool>,
) -> Option<Rc<str>> {
    if let Some(done) = resolved.get(name) {
        return done.clone();
    }
    if depth > MAX_VAR_NESTING {
        too_deep.set(true);
        return None;
    }
    if in_progress.iter().any(|n| n == name) {
        return None;
    }
    let Some(Some(raw)) = raws.get(name) else {
        resolved.insert(name.to_string(), None);
        return None;
    };
    in_progress.push(name.to_string());
    let outer_too_deep = too_deep.replace(false);
    let text = substitute(raw, depth, too_deep, &mut |reference, at| {
        if raws.contains_key(reference) {
            resolve_own(reference, raws, parent, resolved, in_progress, at, too_deep)
                .map(|t| t.to_string())
        } else {
            parent.get(reference).map(str::to_string)
        }
    });
    in_progress.pop();
    let hit_cap = too_deep.get();
    too_deep.set(outer_too_deep || hit_cap);
    let value: Option<Rc<str>> = text.map(Rc::from);
    if !hit_cap || depth == 0 {
        resolved.insert(name.to_string(), value.clone());
    }
    value
}

/// Re-parsed `(property or shorthand, substituted text)` → declarations,
/// for one style pass. Results don't depend on lookup order (determinism).
pub(super) type VarMemo = HashMap<(String, String), Option<Vec<Declaration>>>;

/// Substitute and re-parse every `Value::Unparsed` in `values` (a node's
/// non-custom properties) against `scope`. A failure leaves `unset` for the
/// CSS-wide keyword pass. `report` (the compat probe's pass only) collects
/// the declarations that failed or re-parsed to a keyword the property
/// rejects.
pub(super) fn resolve_var_references(
    values: &mut PropertyMap,
    scope: &CustomScope,
    memo: &RefCell<VarMemo>,
    report: Option<&RefCell<StyleReport>>,
) {
    for (name, value) in values.iter_mut() {
        // The substituted text, kept for the report only.
        let mut reparsed_text = None;
        let resolved = match &*value {
            Value::Unparsed { shorthand, raw } => {
                let parse_as = shorthand.as_deref().unwrap_or(name);
                substitute(raw, 0, &Cell::new(false), &mut |reference, _| {
                    scope.get(reference).map(str::to_string)
                })
                .and_then(|text| {
                    if report.is_some() {
                        reparsed_text = Some(text.clone());
                    }
                    reparse(memo, parse_as, text, name)
                })
            }
            // Never produced by the parser any more (step 1c): invalid.
            Value::Var { .. } => None,
            _ => continue,
        };
        let declared = value.clone();
        // A re-parsed single keyword the property's value kind doesn't
        // take (`color: var(--x)` with `--x: foo`; also an unsupported unit
        // the generic parser keeps as a keyword, `100vw`) would override
        // inheritance with a value nobody reads: treat it as invalid too,
        // counted apart from real failures (`var_keyword_fallback`).
        let rejected_keyword = matches!(
            (&resolved, properties::lookup(name)),
            (Some(Value::Keyword(keyword)), Some(kind)) if kind.is_typed() && !kind.accepts_keyword(keyword)
        );
        match resolved {
            Some(parsed) if !rejected_keyword => *value = parsed,
            _ => {
                if let Some(report) = report {
                    let mut report = report.borrow_mut();
                    let set = if rejected_keyword {
                        &mut report.var_keyword_fallback
                    } else {
                        &mut report.iacvt
                    };
                    let key = declaration_key(name, &declared);
                    set.insert(key.clone());
                    if let Some(text) = reparsed_text {
                        report.reparsed_text.entry(key).or_insert(text);
                    }
                }
                // Invalid at computed-value time = `unset`.
                *value = Value::Keyword("unset".into());
            }
        }
    }
}

/// The value `longhand` gets from parsing `text` as `parse_as`.
fn reparse(memo: &RefCell<VarMemo>, parse_as: &str, text: String, longhand: &str) -> Option<Value> {
    let key = (parse_as.to_string(), text);
    let cached = memo.borrow().get(&key).cloned();
    let declarations = match cached {
        Some(declarations) => declarations,
        None => {
            let parsed = crate::css::parse_property_value(parse_as, &key.1);
            memo.borrow_mut().insert(key, parsed.clone());
            parsed
        }
    }?;
    declarations
        .into_iter()
        .rev()
        .find(|declaration| declaration.name == longhand)
        .map(|declaration| declaration.value)
}

/// What a substitution walk asks for a `var(--name)`: the name and the
/// nesting depth of that reference.
type LookupFn<'a> = dyn FnMut(&str, usize) -> Option<String> + 'a;

/// `raw` with every `var(--name[, fallback])` replaced: the looked-up
/// text, else the (itself substituted) fallback. `None` when a reference
/// fails with no fallback, `var()` is malformed, the result passes
/// [`MAX_SUBSTITUTED_LEN`], or blocks / fallbacks / references nest past
/// [`MAX_VAR_NESTING`] (counted from `depth`; then `too_deep` is set). The
/// walks recurse per level, so the cap is what keeps the stack bounded.
fn substitute(
    raw: &str,
    depth: usize,
    too_deep: &Cell<bool>,
    lookup: &mut LookupFn<'_>,
) -> Option<String> {
    if depth > MAX_VAR_NESTING {
        too_deep.set(true);
        return None;
    }
    let mut parser = CssParser::new(raw);
    let mut out = String::with_capacity(raw.len());
    let start = parser.position();
    let last = substitute_block(&mut parser, &mut out, start, depth, too_deep, lookup)?;
    out.push_str(parser.slice_from(last));
    (out.len() <= MAX_SUBSTITUTED_LEN).then_some(out)
}

/// Walk one block's tokens, copying source text up to each `var(` into
/// `out` and the substitution in its place. Returns the position the next
/// copy starts from (positions are offsets into the one source string, so
/// nested blocks share them).
fn substitute_block(
    parser: &mut CssParser<'_>,
    out: &mut String,
    mut last: SourcePosition,
    depth: usize,
    too_deep: &Cell<bool>,
    lookup: &mut LookupFn<'_>,
) -> Option<SourcePosition> {
    loop {
        let before = parser.position();
        let token = match parser.next_including_whitespace_and_comments() {
            Ok(token) => token.clone(),
            Err(_) => return Some(last),
        };
        let opens_block = matches!(
            token,
            Token::Function(_)
                | Token::ParenthesisBlock
                | Token::SquareBracketBlock
                | Token::CurlyBracketBlock
        );
        if opens_block && depth + 1 > MAX_VAR_NESTING {
            too_deep.set(true);
            return None;
        }
        match token {
            Token::Function(ref f) if f.eq_ignore_ascii_case("var") => {
                out.push_str(parser.slice(last..before));
                let text = parser
                    .parse_nested_block(|inner| {
                        substitute_var(inner, depth + 1, too_deep, lookup)
                            .ok_or(cssparser::ParseError::<()>::custom(()))
                    })
                    .ok()?;
                out.push_str(&text);
                if out.len() > MAX_SUBSTITUTED_LEN {
                    return None;
                }
                last = parser.position();
            }
            _ if opens_block => {
                last = parser
                    .parse_nested_block(|inner| {
                        substitute_block(inner, out, last, depth + 1, too_deep, lookup)
                            .ok_or(cssparser::ParseError::<()>::custom(()))
                    })
                    .ok()?;
            }
            _ => {}
        }
    }
}

/// Inside `var(`: the name, then an optional fallback.
fn substitute_var(
    inner: &mut CssParser<'_>,
    depth: usize,
    too_deep: &Cell<bool>,
    lookup: &mut LookupFn<'_>,
) -> Option<String> {
    let name = match inner.next() {
        Ok(Token::Ident(name)) if name.starts_with("--") => name.to_string(),
        _ => return None,
    };
    if let Some(text) = lookup(&name, depth) {
        // The fallback isn't needed — but `parse_nested_block` parses
        // entirely, so it must still be consumed (unread tokens = error).
        while inner.next_including_whitespace_and_comments().is_ok() {}
        return Some(text);
    }
    inner.expect_comma().ok()?;
    let fallback_start = inner.position();
    while inner.next_including_whitespace_and_comments().is_ok() {}
    let fallback = inner.slice_from(fallback_start).trim().to_string();
    substitute(&fallback, depth + 1, too_deep, lookup)
}

#[cfg(test)]
mod tests {
    //! `substitute` called directly — past the parse-time check — so the
    //! substitution walk's own depth accounting is what's tested. cssparser
    //! bounds nested blocks *within one parser* (75), but every fallback
    //! starts a new parser, so that limit alone doesn't hold here.
    use std::cell::Cell;

    use super::{MAX_VAR_NESTING, substitute};

    fn run(raw: &str) -> (Option<String>, bool) {
        let too_deep = Cell::new(false);
        let out = substitute(raw, 0, &too_deep, &mut |name, _| {
            (name == "--x").then(|| "1px".to_string())
        });
        (out, too_deep.get())
    }

    #[test]
    fn nested_fallbacks_stop_at_the_cap_not_at_cssparsers_per_parser_limit() {
        let nest = |n: usize| format!("{}2px{}", "var(--none, ".repeat(n), ")".repeat(n));
        assert_eq!(run(&nest(MAX_VAR_NESTING / 2)), (Some("2px".into()), false));
        // Each fallback is a new parser: without our count 1,000 levels
        // would recurse 1,000 deep. With it, it stops at the cap.
        assert_eq!(run(&nest(1000)), (None, true));
    }

    #[test]
    fn nested_blocks_count_with_fallbacks() {
        let blocks = |n: usize| format!("{}var(--x){}", "(".repeat(n), ")".repeat(n));
        assert_eq!(
            run(&blocks(10)).0.as_deref(),
            Some("((((((((((1px))))))))))")
        );
        assert_eq!(run(&blocks(MAX_VAR_NESTING + 5)), (None, true));
        // Blocks and fallbacks add up: 20 + 20 > 32.
        let mixed = format!(
            "{}{}3px{}{}",
            "(".repeat(20),
            "var(--none, ".repeat(20),
            ")".repeat(20),
            ")".repeat(20)
        );
        assert_eq!(run(&mixed), (None, true));
    }
}
