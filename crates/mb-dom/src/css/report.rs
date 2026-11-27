//! What the stylesheet parser threw away, for compatibility surveys.
//!
//! The parser is tolerant: a rule whose selector it can't read, a
//! declaration whose value it can't read and every at-rule except `@media`
//! and `@layer` are dropped silently (that's the CSS error-recovery contract). On a real
//! site those silent drops are most of what goes wrong, so
//! [`parse_with_report`] runs the same parse and also hands back the dropped
//! pieces, each with a [`DropReason`]. Normal parsing never records (or
//! classifies) anything.
//!
//! [`DropReason::origin`] splits drops into **ours** (Chrome would keep it —
//! an engine gap to close) and **browser-too** (Chrome drops it as well — not
//! a target). The split is frozen for compat round 3
//! (docs/compat-round3.md "0b 분류표"); change the doc before the code.

use std::cell::RefCell;

use cssparser::Parser as CssParser;

use super::Stylesheet;
use super::media;
use super::properties::{self, ValueKind};
use super::supports;

/// Longest source excerpt kept per dropped item.
const EXCERPT_LIMIT: usize = 160;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParseReport {
    /// Rules dropped because the selector didn't parse.
    pub dropped_rules: Vec<DroppedRule>,
    /// Declarations dropped because the value (or the name) didn't parse —
    /// including nested rules inside a block, which the declaration parser
    /// sees as one broken declaration ([`DropReason::NestedRule`]).
    pub dropped_declarations: Vec<DroppedDeclaration>,
    /// Every at-rule seen, in source order.
    pub at_rules: Vec<AtRuleSeen>,
    /// Indexes (into `at_rules`) of the kept blocks being parsed, outer
    /// first — how a nested at-rule finds its [`AtRuleSeen::parent`].
    open_blocks: Vec<usize>,
}

/// Why something was dropped. Mechanical (what the parser hit), checked in
/// the order the variants are listed for declarations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DropReason {
    /// The block held a nested rule (`&:hover{…}`, `.child{…}`) — CSS
    /// nesting, which Chrome supports and we don't parse.
    NestedRule,
    /// No browser knows the property: a typo, `-moz-`/`-ms-`/`-o-`, an
    /// unknown `-webkit-` name, an IE hack name (`*zoom`).
    UnknownProperty,
    /// A known property with an old-IE value hack (`progid:`,
    /// `expression(`, `\9`, `\0`, `!ie`) — invalid in every modern browser.
    LegacyHack,
    /// The value carries `!important` and the same declaration without it
    /// parses — the flag itself broke it (before step 1a: every
    /// `!important`; since: a bug).
    Important,
    /// A known property without a dedicated parser got more than one
    /// token (`border: 1px solid`, comma lists, `calc(a) b`).
    MultiToken,
    /// A known property's value didn't parse for another reason (a
    /// dedicated parser rejected it, or one token nothing accepts). Chrome
    /// may or may not accept it; ambiguous counts as ours.
    InvalidValue,
    /// A rule whose selector uses something we don't parse.
    UnsupportedSelector,
    /// A selector with another engine's prefix (`::-moz-selection`,
    /// `:-ms-input-placeholder`) — Chrome drops the whole rule too.
    ForeignSelector,
    /// An at-rule whose body is skipped (`@supports`, `@font-face`, an
    /// invalid `@layer`…).
    SkippedAtRule,
    /// An at-rule with another engine's prefix (`@-moz-document`).
    ForeignAtRule,
}

/// Whether a drop is an engine gap ([`Origin::Ours`]) or what a real
/// browser does too ([`Origin::BrowserToo`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Origin {
    Ours,
    BrowserToo,
}

impl DropReason {
    /// Stable short code for reports (`summary.tsv`, `css.txt`).
    pub fn code(self) -> &'static str {
        match self {
            DropReason::NestedRule => "nested-rule",
            DropReason::UnknownProperty => "unknown-property",
            DropReason::LegacyHack => "legacy-hack",
            DropReason::Important => "important",
            DropReason::MultiToken => "multi-token",
            DropReason::InvalidValue => "invalid-value",
            DropReason::UnsupportedSelector => "unsupported-selector",
            DropReason::ForeignSelector => "foreign-selector",
            DropReason::SkippedAtRule => "skipped-at-rule",
            DropReason::ForeignAtRule => "foreign-at-rule",
        }
    }

    /// The frozen ours / browser-too table.
    pub fn origin(self) -> Origin {
        match self {
            DropReason::UnknownProperty
            | DropReason::LegacyHack
            | DropReason::ForeignSelector
            | DropReason::ForeignAtRule => Origin::BrowserToo,
            DropReason::NestedRule
            | DropReason::Important
            | DropReason::MultiToken
            | DropReason::InvalidValue
            | DropReason::UnsupportedSelector
            | DropReason::SkippedAtRule => Origin::Ours,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DroppedDeclaration {
    /// Lower-cased property name as written (`""` for a nested rule).
    pub property: String,
    /// `name: value` excerpt.
    pub text: String,
    pub reason: DropReason,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DroppedRule {
    /// Selector excerpt (the end is kept: that's where it usually fails).
    pub selector: String,
    pub reason: DropReason,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AtRuleSeen {
    /// Lower-cased, without the `@`.
    pub name: String,
    /// The text between the name and the block (`(min-width: 768px)`).
    pub prelude: String,
    /// Whether the body's rules were kept (`@media` — evaluated per style
    /// pass, see [`media_verdict`]; `@layer`, statement form included) or
    /// the whole rule skipped.
    pub kept: bool,
    /// Size of a skipped body in source bytes (0 when kept or block-less).
    pub skipped_bytes: usize,
    /// Rules kept from a kept body, nested blocks' rules included.
    pub rules: usize,
    /// Index in [`ParseReport::at_rules`] of the kept block (`@media`,
    /// `@layer`) this one sits in (`None` at the top level).
    pub parent: Option<usize>,
    /// `false` for the statement form (`@layer a, b;`, `@import …;`).
    pub has_block: bool,
}

/// What the engine does with one `@media` block, for surveys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaVerdict {
    /// Whether the block's rules apply at the given viewport (on its own;
    /// an enclosing block is evaluated separately).
    pub applies: bool,
    /// Comma-separated queries in the prelude that failed to parse (each
    /// one counts as `not all`).
    pub invalid_queries: usize,
}

/// How the engine treats a `@media` block with this prelude at a
/// `viewport_width` × `viewport_height` CSS px viewport (light theme) —
/// the same parse and [`media::evaluate`] the cascade uses.
pub fn media_verdict(prelude: &str, viewport_width: f32, viewport_height: f32) -> MediaVerdict {
    let list = media::MediaQueryList::parse(prelude);
    MediaVerdict {
        applies: media::evaluate(
            &list,
            &media::MediaContext::new(viewport_width, viewport_height),
        ),
        invalid_queries: list.invalid_queries(),
    }
}

/// Whether the engine applies the body of a `@supports` block with this
/// prelude (on its own; an enclosing block is evaluated separately) — the
/// same parse and [`supports::evaluate`] the stylesheet parser uses. An
/// invalid prelude is false (the rule is dropped).
pub fn supports_verdict(prelude: &str) -> bool {
    supports::SupportsCondition::parse(prelude).is_some_and(|c| supports::evaluate(&c))
}

impl AtRuleSeen {
    /// Why the rule was dropped, or `None` when nothing was lost: a kept
    /// body, or `@charset` (an encoding hint, no content).
    pub fn drop_reason(&self) -> Option<DropReason> {
        if self.kept || self.name == "charset" {
            None
        } else if properties::is_foreign_vendor_prefix(&self.name) {
            Some(DropReason::ForeignAtRule)
        } else {
            Some(DropReason::SkippedAtRule)
        }
    }
}

thread_local! {
    static REPORT: RefCell<Option<ParseReport>> = const { RefCell::new(None) };
}

/// [`super::parse`] plus the list of what it dropped.
pub fn parse_with_report(source: &str) -> (Stylesheet, ParseReport) {
    REPORT.with(|slot| *slot.borrow_mut() = Some(ParseReport::default()));
    let sheet = super::parse(source).unwrap_or_default();
    let report = REPORT
        .with(|slot| slot.borrow_mut().take())
        .unwrap_or_default();
    (sheet, report)
}

pub(super) fn record(update: impl FnOnce(&mut ParseReport)) {
    REPORT.with(|slot| {
        if let Some(report) = slot.borrow_mut().as_mut() {
            update(report);
        }
    });
}

pub(super) fn is_active() -> bool {
    REPORT.with(|slot| slot.borrow().is_some())
}

/// Note an at-rule; returns its index in [`ParseReport::at_rules`] (`None`
/// when no report is being collected).
pub(super) fn record_at_rule(name: &str, prelude: &str, kept: bool) -> Option<usize> {
    REPORT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let report = slot.as_mut()?;
        let name = name.to_ascii_lowercase();
        // Conditions and layer name lists are kept whole: the probe
        // re-reads them.
        let prelude = if matches!(name.as_str(), "media" | "supports" | "layer") {
            excerpt_unbounded(prelude)
        } else {
            excerpt(prelude)
        };
        report.at_rules.push(AtRuleSeen {
            name,
            prelude,
            kept,
            skipped_bytes: 0,
            rules: 0,
            parent: report.open_blocks.last().copied(),
            has_block: true,
        });
        Some(report.at_rules.len() - 1)
    })
}

/// Note that the at-rule just recorded had no block (`@layer a, b;`).
pub(super) fn mark_block_less() {
    record(|r| {
        if let Some(seen) = r.at_rules.last_mut() {
            seen.has_block = false;
        }
    });
}

/// An at-rule whose body is not kept after all (an invalid
/// `@layer a, b { }`), or a skipped one's body size: `seen` (or, without
/// an index, the last at-rule recorded) is not kept and lost `bytes`.
pub(super) fn skip_body(seen: Option<usize>, bytes: usize) {
    record(|r| {
        let at_rule = match seen {
            Some(index) => r.at_rules.get_mut(index),
            None => r.at_rules.last_mut(),
        };
        if let Some(at_rule) = at_rule {
            at_rule.kept = false;
            at_rule.skipped_bytes = bytes;
        }
    });
}

/// Bracket the parse of a kept block's body: `enter` before, `leave`
/// with the number of rules it produced after.
pub(super) fn enter_block(index: Option<usize>) {
    if let Some(index) = index {
        record(|r| r.open_blocks.push(index));
    }
}

pub(super) fn leave_block(index: Option<usize>, rules: usize) {
    if let Some(index) = index {
        record(|r| {
            r.open_blocks.pop();
            if let Some(seen) = r.at_rules.get_mut(index) {
                seen.rules = rules;
            }
        });
    }
}

/// Classify a declaration the parser rejected, from its full source text.
pub(super) fn dropped_declaration(source: &str) -> DroppedDeclaration {
    let source = source.trim().trim_end_matches(';');
    let text = excerpt(source);
    if source.contains('{') {
        return DroppedDeclaration {
            property: String::new(),
            text,
            reason: DropReason::NestedRule,
        };
    }
    let (name, value) = source.split_once(':').unwrap_or((source, ""));
    let property = name.trim().to_ascii_lowercase();
    let reason = declaration_reason(&property, value);
    DroppedDeclaration {
        property,
        text,
        reason,
    }
}

fn declaration_reason(property: &str, value: &str) -> DropReason {
    let Some(kind) = properties::lookup(property) else {
        return DropReason::UnknownProperty;
    };
    let lower = value.to_ascii_lowercase();
    if ["progid:", "expression(", "\\9", "\\0", "!ie"]
        .iter()
        .any(|hack| lower.contains(hack))
    {
        return DropReason::LegacyHack;
    }
    // A trailing `!important`: blame the flag only if the value before it
    // is accepted on its own; otherwise classify that value (step 1a — the
    // flag is parsed, so a drop that carries it is usually the value's).
    if let Some(before) = strip_trailing_important(value) {
        if parses_alone(property, before) {
            return DropReason::Important;
        }
        return value_reason(kind, before);
    }
    if has_important(&lower) {
        // `!important` followed by more tokens: invalid everywhere.
        return DropReason::InvalidValue;
    }
    value_reason(kind, value)
}

fn value_reason(kind: ValueKind, value: &str) -> DropReason {
    if kind != ValueKind::Dedicated && top_level_tokens(value) > 1 {
        return DropReason::MultiToken;
    }
    DropReason::InvalidValue
}

/// `value` without a final `!important` (whitespace/comments allowed
/// around and inside it), or `None` when it doesn't end with one.
fn strip_trailing_important(value: &str) -> Option<&str> {
    let lower = value.to_ascii_lowercase();
    let at = lower.rfind('!')?;
    let rest = lower[at + 1..].replace("/**/", " ");
    (rest.trim() == "important").then(|| &value[..at])
}

/// Whether `property: value` is a declaration the parser keeps. Runs with
/// the report put aside so the probe parse doesn't record anything.
fn parses_alone(property: &str, value: &str) -> bool {
    let saved = REPORT.with(|slot| slot.borrow_mut().take());
    let kept = !super::parse_inline_style(&format!("{property}:{value}")).is_empty();
    REPORT.with(|slot| *slot.borrow_mut() = saved);
    kept
}

/// `!important` with any whitespace or comment between `!` and the word.
fn has_important(lower_value: &str) -> bool {
    lower_value.match_indices('!').any(|(at, _)| {
        lower_value[at + 1..]
            .trim_start()
            .trim_start_matches("/**/")
            .trim_start()
            .starts_with("important")
    })
}

/// Tokens at the top level of a value (a function or block is one).
fn top_level_tokens(value: &str) -> usize {
    let mut parser = CssParser::new(value);
    let mut count = 0;
    while parser.next().is_ok() {
        count += 1;
    }
    count
}

/// Classify a rule whose selector didn't parse.
pub(super) fn dropped_rule(source: &str) -> DroppedRule {
    let prelude = source
        .split('{')
        .next()
        .unwrap_or(source)
        .to_ascii_lowercase();
    let foreign = ["::-moz-", ":-moz-", "::-ms-", ":-ms-", "::-o-", ":-o-"]
        .iter()
        .any(|prefix| prelude.contains(prefix));
    DroppedRule {
        selector: rule_prelude(source),
        reason: if foreign {
            DropReason::ForeignSelector
        } else {
            DropReason::UnsupportedSelector
        },
    }
}

fn excerpt_unbounded(source: &str) -> String {
    source.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn excerpt(source: &str) -> String {
    let flat = excerpt_unbounded(source);
    match flat.char_indices().nth(EXCERPT_LIMIT) {
        Some((cut, _)) => format!("{}…", &flat[..cut]),
        None => flat,
    }
}

/// The selector part of a dropped rule's source (everything before `{`).
fn rule_prelude(source: &str) -> String {
    let prelude = source.split('{').next().unwrap_or(source);
    // Keep the end: the part a selector fails on is usually the last
    // compound (`…:before`), which a front excerpt would cut off.
    let flat = excerpt_unbounded(prelude);
    match flat.char_indices().rev().nth(EXCERPT_LIMIT) {
        Some((cut, _)) => format!("…{}", &flat[cut..]),
        None => flat,
    }
}

#[cfg(test)]
mod tests {
    use super::{DropReason, Origin, dropped_declaration, dropped_rule, parse_with_report};

    #[test]
    fn report_lists_dropped_rules_declarations_and_at_rules() {
        let (sheet, report) = parse_with_report(
            ".a { width: 1px !imp; color: red }
             .b:frobnicate { color: blue }
             @media (min-width: 1px) { .c { colour-ish: 1 2 3 } }
             @supports (display: grid) { .d { display: grid } }",
        );
        assert_eq!(
            sheet.rules.len(),
            3,
            "the .a, @media .c and @supports .d rules"
        );
        let rules: Vec<_> = report
            .dropped_rules
            .iter()
            .map(|r| (r.selector.as_str(), r.reason))
            .collect();
        assert_eq!(rules, [(".b:frobnicate", DropReason::UnsupportedSelector)]);
        let decls: Vec<_> = report
            .dropped_declarations
            .iter()
            .map(|d| (d.property.as_str(), d.text.as_str(), d.reason))
            .collect();
        assert_eq!(
            decls,
            [
                ("width", "width: 1px !imp", DropReason::MultiToken),
                (
                    "colour-ish",
                    "colour-ish: 1 2 3",
                    DropReason::UnknownProperty
                )
            ]
        );
        let at_rules: Vec<_> = report
            .at_rules
            .iter()
            .map(|a| (a.name.as_str(), a.prelude.as_str(), a.kept, a.skipped_bytes))
            .collect();
        assert_eq!(
            at_rules,
            [
                ("media", "(min-width: 1px)", true, 0),
                ("supports", "(display: grid)", true, 0)
            ]
        );
    }

    #[test]
    fn media_blocks_count_their_rules_and_know_their_parent() {
        let (_, report) = parse_with_report(
            "@media screen { .a{} @supports (x:y) { .s{} } @media (min-width: 1px) { .b{} .c{} } .d{} }
             @media print { .e{} }",
        );
        let seen: Vec<_> = report
            .at_rules
            .iter()
            .map(|a| (a.name.as_str(), a.rules, a.parent))
            .collect();
        assert_eq!(
            seen,
            [
                ("media", 4, None),
                // A false `@supports` body is parsed (its rule counted
                // here) and dropped (not in the enclosing block's 4).
                ("supports", 1, Some(0)),
                ("media", 2, Some(0)),
                ("media", 1, None)
            ]
        );
    }

    #[test]
    fn layer_and_supports_at_rules_are_recorded() {
        let long = format!("(display: grid){}", " and (gap: 1px)".repeat(20));
        let (_, report) = parse_with_report(&format!(
            "@layer reset, base; @layer base {{ .a{{}} }} @layer {{ .b{{}} }} @supports {long} {{ .c{{}} }}"
        ));
        let seen: Vec<_> = report
            .at_rules
            .iter()
            .map(|a| (a.name.as_str(), a.prelude.as_str(), a.has_block))
            .collect();
        assert_eq!(
            seen,
            [
                ("layer", "reset, base", false),
                ("layer", "base", true),
                ("layer", "", true),
                ("supports", long.as_str(), true),
            ]
        );
    }

    #[test]
    fn layer_blocks_are_kept_and_enclose_their_at_rules() {
        let (_, report) = parse_with_report(
            "@layer a, b; @layer a { .x{} @media screen { .y{} } @layer c { .z{} } }
             @layer a b { .bad{} } @layer;",
        );
        let seen: Vec<_> = report
            .at_rules
            .iter()
            .map(|a| {
                (
                    a.prelude.as_str(),
                    a.kept,
                    a.rules,
                    a.parent,
                    a.drop_reason(),
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("a, b", true, 0, None, None),
                ("a", true, 3, None, None),
                ("screen", true, 1, Some(1), None),
                ("c", true, 1, Some(1), None),
                ("a b", false, 0, None, Some(DropReason::SkippedAtRule)),
                ("", false, 0, None, Some(DropReason::SkippedAtRule)),
            ]
        );
    }

    #[test]
    fn media_prelude_is_kept_whole() {
        let long = format!("(min-width: 1px){}", ", print".repeat(40));
        let (_, report) = parse_with_report(&format!("@media {long} {{ .a{{}} }}"));
        assert_eq!(report.at_rules[0].prelude, long);
    }

    #[test]
    fn plain_parse_records_nothing() {
        let _ = super::super::parse(".b:frobnicate { color: blue }");
        let (_, report) = parse_with_report(".a { color: red }");
        assert!(report.dropped_rules.is_empty());
    }

    #[test]
    fn declaration_reasons_follow_the_frozen_order() {
        let reason = |source: &str| dropped_declaration(source).reason;
        assert_eq!(reason("&:hover { color: red }"), DropReason::NestedRule);
        assert_eq!(
            reason("-moz-appearance: none !important"),
            DropReason::UnknownProperty
        );
        assert_eq!(reason("*zoom: 1"), DropReason::UnknownProperty);
        assert_eq!(reason("-webkit-frobnicate: 1"), DropReason::UnknownProperty);
        assert_eq!(
            reason("filter: progid:DXImageTransform.Microsoft.gradient(enabled=false)"),
            DropReason::LegacyHack
        );
        assert_eq!(reason("width: 100px\\9"), DropReason::LegacyHack);
        // `!important` is parsed (step 1a): a drop that carries it is
        // classified by the value before it — `important` would mean the
        // flag itself broke an otherwise valid declaration (a bug).
        assert_eq!(
            reason("outline: 1px solid red !important"),
            DropReason::MultiToken
        );
        assert_eq!(
            reason("width: calc(100% - 2px) ! important"),
            DropReason::InvalidValue
        );
        assert_eq!(
            reason("color: red !important foo"),
            DropReason::InvalidValue
        );
        assert_eq!(reason("outline: 1px solid red"), DropReason::MultiToken);
        // `border` has a dedicated parser since step 1b: a value it rejects
        // is `invalid-value`, not `multi-token`.
        assert_eq!(
            reason("border: 1px solid red blue"),
            DropReason::InvalidValue
        );
        assert_eq!(reason("--shadow: 0 1px 2px red"), DropReason::MultiToken);
        assert_eq!(
            reason("-webkit-transition: opacity .2s"),
            DropReason::MultiToken
        );
        assert_eq!(
            reason("margin: 1px 2px 3px 4px 5px"),
            DropReason::InvalidValue
        );
        assert_eq!(reason("width: calc(100% - 2px)"), DropReason::InvalidValue);
        assert_eq!(dropped_declaration("Color: red;").property, "color");
    }

    #[test]
    fn origin_table_is_frozen() {
        use DropReason::*;
        let ours = [
            NestedRule,
            Important,
            MultiToken,
            InvalidValue,
            UnsupportedSelector,
            SkippedAtRule,
        ];
        let browser_too = [UnknownProperty, LegacyHack, ForeignSelector, ForeignAtRule];
        assert!(ours.iter().all(|r| r.origin() == Origin::Ours));
        assert!(browser_too.iter().all(|r| r.origin() == Origin::BrowserToo));
    }

    #[test]
    fn foreign_selectors_and_at_rules_are_browser_too() {
        assert_eq!(
            dropped_rule("a::-moz-selection, a::selection { }").reason,
            DropReason::ForeignSelector
        );
        assert_eq!(
            dropped_rule("input:-ms-input-placeholder{}").reason,
            DropReason::ForeignSelector
        );
        assert_eq!(
            dropped_rule("::-webkit-scrollbar{}").reason,
            DropReason::UnsupportedSelector
        );
        // (`@charset` never reaches the at-rule parser: cssparser eats it.)
        // An invalid `@supports` prelude drops the rule (ours, as filed).
        let (_, report) = parse_with_report(
            "@charset \"utf-8\"; @-moz-document url-prefix() { a{} } @font-face { src: url(a) }
             @supports display: grid { a{} }",
        );
        let reasons: Vec<_> = report.at_rules.iter().map(|a| a.drop_reason()).collect();
        assert_eq!(
            reasons,
            [
                Some(DropReason::ForeignAtRule),
                Some(DropReason::SkippedAtRule),
                Some(DropReason::SkippedAtRule)
            ]
        );
    }
}
