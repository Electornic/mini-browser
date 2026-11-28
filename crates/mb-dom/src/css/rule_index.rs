//! Selector rule index (compat round 3 follow-up F4).
//!
//! The cascade used to try every selector of every rule on every node —
//! O(rules × nodes); GitHub's 14,000 rules took ~1 s per style pass. As
//! browsers do, each selector is filed once, at parse time, under one
//! simple selector its **rightmost compound requires**: its id, else a
//! class, else its tag name, else the universal list. A node then only
//! tries the selectors filed under its own id, its classes and its tag,
//! plus the universal ones — the rest cannot match it.
//!
//! The keys follow `dom_select`'s matching exactly, so the result is the
//! same set of matching selectors: ids and classes compare case-sensitively
//! (the cascade matches in no-quirks mode), class tokens split like
//! `has_class` (`split_whitespace`), tag names compare ASCII
//! case-insensitively (bucketed lower-cased). Only top-level components of
//! the rightmost compound count — `.a` inside `:not(.a)`, `:is(.a, .b)` or
//! `:where(.a)` requires nothing (those go by their other components, else
//! universal). Selectors with a pseudo-element are not filed at all: they
//! never match an element.
//!
//! Lookups only: nothing iterates the maps (determinism); candidates are
//! sorted by (rule, selector) before matching.

use std::collections::HashMap;

use selectors::parser::Component;

use super::{MiniBrowserSelectorImpl, Rule};

/// A selector: (rule index in the sheet, index in the rule's list).
pub type IndexEntry = (u32, u32);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RuleIndex {
    by_id: HashMap<String, Vec<IndexEntry>>,
    by_class: HashMap<String, Vec<IndexEntry>>,
    by_tag: HashMap<String, Vec<IndexEntry>>,
    universal: Vec<IndexEntry>,
}

enum Key {
    Id(String),
    Class(String),
    Tag(String),
    Universal,
}

/// The bucket a selector goes in: the rarest thing its rightmost compound
/// requires (id > class > tag).
fn key(selector: &selectors::parser::Selector<MiniBrowserSelectorImpl>) -> Key {
    let mut class = None;
    let mut tag = None;
    for component in selector.iter() {
        match component {
            Component::ID(id) => return Key::Id(id.0.clone()),
            Component::Class(name) if class.is_none() => class = Some(name.0.clone()),
            Component::LocalName(local) if tag.is_none() => {
                tag = Some(local.lower_name.0.to_ascii_lowercase())
            }
            _ => {}
        }
    }
    match (class, tag) {
        (Some(class), _) => Key::Class(class),
        (None, Some(tag)) => Key::Tag(tag),
        (None, None) => Key::Universal,
    }
}

impl RuleIndex {
    /// Index a sheet's rules (rule and selector positions past `u32` are
    /// filed as universal — matched, just not skipped).
    pub fn build(rules: &[Rule]) -> Self {
        let mut index = RuleIndex::default();
        for (rule_i, rule) in rules.iter().enumerate() {
            for (selector_i, selector) in rule.selectors.list().slice().iter().enumerate() {
                let (Ok(rule_i), Ok(selector_i)) =
                    (u32::try_from(rule_i), u32::try_from(selector_i))
                else {
                    continue;
                };
                // A selector with a pseudo-element (`a::before`) can never
                // match an element (step 4: parsed, no generated content):
                // filing it would only make every node try it. The flat
                // index keeps it — and it never matches there either.
                if selector.has_pseudo_element() {
                    continue;
                }
                let entry = (rule_i, selector_i);
                match key(selector) {
                    Key::Id(id) => index.by_id.entry(id).or_default().push(entry),
                    Key::Class(class) => index.by_class.entry(class).or_default().push(entry),
                    Key::Tag(tag) => index.by_tag.entry(tag).or_default().push(entry),
                    Key::Universal => index.universal.push(entry),
                }
            }
        }
        index
    }

    /// Every selector in the universal list — the cascade then tries each
    /// one on every node, as before the index (tests and the fuzzer compare
    /// the two).
    pub fn flat(rules: &[Rule]) -> Self {
        let mut index = RuleIndex::default();
        for (rule_i, rule) in rules.iter().enumerate() {
            for selector_i in 0..rule.selectors.list().slice().len() {
                if let (Ok(r), Ok(s)) = (u32::try_from(rule_i), u32::try_from(selector_i)) {
                    index.universal.push((r, s));
                }
            }
        }
        index
    }

    /// Append to `out` the selectors that may match an element with this
    /// `id` attribute, `class` attribute and tag name (`None` for a
    /// non-element: only the universal list). Unsorted, may repeat.
    pub fn candidates(
        &self,
        id: Option<&str>,
        class: Option<&str>,
        tag: Option<&str>,
        out: &mut Vec<IndexEntry>,
    ) {
        out.extend_from_slice(&self.universal);
        if let Some(found) = id.and_then(|id| self.by_id.get(id)) {
            out.extend_from_slice(found);
        }
        if let Some(class) = class {
            for token in class.split_whitespace() {
                if let Some(found) = self.by_class.get(token) {
                    out.extend_from_slice(found);
                }
            }
        }
        if let Some(found) = tag.and_then(|tag| self.by_tag.get(&tag.to_ascii_lowercase())) {
            out.extend_from_slice(found);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::parse;

    fn candidates(css: &str, id: Option<&str>, class: Option<&str>, tag: &str) -> Vec<(u32, u32)> {
        let sheet = parse(css).unwrap();
        let mut out = Vec::new();
        sheet.index.candidates(id, class, Some(tag), &mut out);
        out.sort_unstable();
        out.dedup();
        out
    }

    #[test]
    fn selectors_are_filed_under_their_rightmost_compound() {
        let css = "#x {} .a .b {} div.c {} SPAN {} * {} [title] {} :not(.q) {} .a, #y {}";
        // rule 0 #x, 1 .b, 2 .c, 3 span, 4 *, 5 [title], 6 :not(.q), 7 (.a | #y)
        assert_eq!(
            candidates(css, Some("x"), Some("b c"), "div"),
            [(0, 0), (1, 0), (2, 0), (4, 0), (5, 0), (6, 0)]
        );
        assert_eq!(
            candidates(css, Some("y"), Some("a"), "span"),
            [(3, 0), (4, 0), (5, 0), (6, 0), (7, 0), (7, 1)]
        );
        // `.a .b` needs `.b` on the subject; `.a` alone doesn't bring it.
        assert!(!candidates(css, None, Some("a"), "p").contains(&(1, 0)));
        // Ids and classes are case-sensitive, tags are not.
        assert!(!candidates(css, Some("X"), None, "p").contains(&(0, 0)));
        assert!(candidates(css, None, None, "span").contains(&(3, 0)));
    }

    #[test]
    fn is_where_arguments_are_not_keys_and_pseudo_elements_are_not_filed() {
        // rule 0 :is(.a,.b) 1 .c:is(#x) 2 :where(p) span 3 a::before 4 (.d::after | .d)
        let css = ":is(.a, .b) {} .c:is(#x) {} :where(p) span {} a::before {} .d::after, .d {}";
        // `:is(.a, .b)` is filed as universal: an element with neither class
        // still gets it as a candidate (and fails to match it).
        assert!(candidates(css, None, None, "div").contains(&(0, 0)));
        // `.c:is(#x)` requires `.c` (top level), not `#x` (an argument).
        assert!(candidates(css, None, Some("c"), "div").contains(&(1, 0)));
        assert!(!candidates(css, Some("x"), None, "div").contains(&(1, 0)));
        assert!(candidates(css, None, None, "span").contains(&(2, 0)));
        // A selector with a pseudo-element can never match an element: in
        // no bucket at all.
        assert!(!candidates(css, None, Some("d"), "a").contains(&(3, 0)));
        let d = candidates(css, None, Some("d"), "a");
        assert!(d.contains(&(4, 1)) && !d.contains(&(4, 0)));
    }
}
