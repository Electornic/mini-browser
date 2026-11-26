//! What the stylesheet parser threw away, for compatibility surveys.
//!
//! The parser is tolerant: a rule whose selector it can't read, a
//! declaration whose value it can't read and every at-rule except `@media`
//! are dropped silently (that's the CSS error-recovery contract). On a real
//! site those silent drops are most of what goes wrong, so
//! [`parse_with_report`] runs the same parse and also hands back the dropped
//! pieces. Normal parsing never records anything.

use std::cell::RefCell;

use super::Stylesheet;

/// Longest source excerpt kept per dropped item.
const EXCERPT_LIMIT: usize = 160;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParseReport {
    /// Selector text of rules dropped because the selector didn't parse.
    pub dropped_rules: Vec<String>,
    /// `name: value` text of declarations dropped because the value didn't
    /// parse (including any `!important` ones — the flag isn't modelled).
    pub dropped_declarations: Vec<String>,
    /// Every at-rule seen, in source order.
    pub at_rules: Vec<AtRuleSeen>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AtRuleSeen {
    /// Lower-cased, without the `@`.
    pub name: String,
    /// The text between the name and the block (`(min-width: 768px)`).
    pub prelude: String,
    /// Whether the body's rules were kept (`@media`, always treated as
    /// matching) or the whole rule skipped.
    pub kept: bool,
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
pub(super) fn rule_prelude(source: &str) -> String {
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
    use super::parse_with_report;

    #[test]
    fn report_lists_dropped_rules_declarations_and_at_rules() {
        let (sheet, report) = parse_with_report(
            ".a { width: 1px !important; color: red }
             .b:frobnicate { color: blue }
             @media (min-width: 1px) { .c { colour-ish: 1 2 3 } }
             @supports (display: grid) { .d { display: grid } }",
        );
        assert_eq!(sheet.rules.len(), 2, "the .a rule and the @media .c rule");
        assert_eq!(report.dropped_rules, vec![".b:frobnicate".to_string()]);
        assert_eq!(
            report.dropped_declarations,
            vec![
                "width: 1px !important".to_string(),
                "colour-ish: 1 2 3".to_string()
            ]
        );
        let at_rules: Vec<_> = report
            .at_rules
            .iter()
            .map(|a| (a.name.as_str(), a.prelude.as_str(), a.kept))
            .collect();
        assert_eq!(
            at_rules,
            [
                ("media", "(min-width: 1px)", true),
                ("supports", "(display: grid)", false)
            ]
        );
    }

    #[test]
    fn plain_parse_records_nothing() {
        let _ = super::super::parse(".b:frobnicate { color: blue }");
        let (_, report) = parse_with_report(".a { color: red }");
        assert!(report.dropped_rules.is_empty());
    }
}
