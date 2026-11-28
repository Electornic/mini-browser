// Selector glue for the `selectors` crate. Defines the
// `MiniBrowserSelectorImpl` flavour (pseudo-classes, namespace-free
// identifier type, never-matching pseudo-elements), the owned `Selector`
// newtype wrapping the parsed `SelectorList` (with the combinator nesting
// cap), and the `SelectorParserImpl` callback that maps recognised
// pseudo-class and pseudo-element names and turns on `:is()`/`:where()`. The rest of
// css/mod.rs treats this as an opaque toolkit — it imports `Selector`
// and (via `pub(super)`) `SelectorParserImpl` to feed into
// `SelectorList::parse`.

use std::borrow::Borrow;
use std::fmt;

use cssparser::{ParseError as CssParseError, ToCss};
use precomputed_hash::PrecomputedHash;
use selectors::parser::Component;
use selectors::parser::SelectorList as SelectorsSelectorList;

/// Selector implementation flavour for the `selectors` crate. We don't need
/// namespaces, custom case sensitivity or extra matching data, so most
/// associated types are stringy or unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MiniBrowserSelectorImpl;

/// Stringy identifier with a precomputed hash. Selectors crate requires
/// `PrecomputedHash` on `Identifier` / `LocalName` for the bloom-filter
/// optimisation; the cheapest impl is to wrap `String` and recompute on
/// demand. This isn't hot in practice — the AST is built once at parse
/// time and only the selectors-crate internals call `precomputed_hash`.
#[derive(Debug, Clone, Default, Eq, Hash, PartialEq, PartialOrd, Ord)]
pub struct CssString(pub String);

impl CssString {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CssString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'a> From<&'a str> for CssString {
    fn from(s: &'a str) -> Self {
        CssString(s.to_owned())
    }
}

impl AsRef<str> for CssString {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for CssString {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl PrecomputedHash for CssString {
    fn precomputed_hash(&self) -> u32 {
        // Cheap FNV-1a; the selectors crate only uses these for bloom-filter
        // ancestor fast-rejection, so it just needs to be a stable hash.
        let mut hash: u32 = 0x811c9dc5;
        for byte in self.0.as_bytes() {
            hash ^= *byte as u32;
            hash = hash.wrapping_mul(0x01000193);
        }
        hash
    }
}

impl ToCss for CssString {
    fn to_css<W>(&self, dest: &mut W) -> fmt::Result
    where
        W: fmt::Write,
    {
        dest.write_str(&self.0)
    }
}

/// Non-tree-structural pseudo-classes we recognise. `:hover`/`:focus`/`:active`
/// /`:focus-visible`/`:focus-within` pull live state from `MatchingState`
/// (`dom_select.rs`); `:link` / `:visited` are wired to "anchor with href" /
/// "never matches" the same way the previous hand-rolled matcher did;
/// `:checked`/`:disabled`/`:enabled` read attributes — in this engine an
/// element's current state *is* its attributes (compat round 3 step 4b).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonTSPseudoClass {
    Hover,
    Focus,
    Active,
    Link,
    Visited,
    FocusVisible,
    FocusWithin,
    Checked,
    Disabled,
    Enabled,
}

impl ToCss for NonTSPseudoClass {
    fn to_css<W>(&self, dest: &mut W) -> fmt::Result
    where
        W: fmt::Write,
    {
        dest.write_str(match self {
            NonTSPseudoClass::Hover => ":hover",
            NonTSPseudoClass::Focus => ":focus",
            NonTSPseudoClass::Active => ":active",
            NonTSPseudoClass::Link => ":link",
            NonTSPseudoClass::Visited => ":visited",
            NonTSPseudoClass::FocusVisible => ":focus-visible",
            NonTSPseudoClass::FocusWithin => ":focus-within",
            NonTSPseudoClass::Checked => ":checked",
            NonTSPseudoClass::Disabled => ":disabled",
            NonTSPseudoClass::Enabled => ":enabled",
        })
    }
}

impl selectors::parser::NonTSPseudoClass for NonTSPseudoClass {
    fn is_active_or_hover(&self) -> bool {
        matches!(self, NonTSPseudoClass::Active | NonTSPseudoClass::Hover)
    }

    fn is_user_action_state(&self) -> bool {
        matches!(
            self,
            NonTSPseudoClass::Hover
                | NonTSPseudoClass::Active
                | NonTSPseudoClass::Focus
                | NonTSPseudoClass::FocusVisible
                | NonTSPseudoClass::FocusWithin
        )
    }
}

/// Pseudo-elements we parse (compat round 3 step 4). None is rendered —
/// no generated content — and none ever matches an element
/// (`match_pseudo_element` is always false): they exist so a comma list
/// like `a::before, .x` keeps `.x` instead of losing the whole rule.
/// Every `::-webkit-*` name is valid (the Selectors 4 legacy rule — Chrome
/// does the same); other vendor prefixes (`::-moz-*`) still fail to parse,
/// as in Chrome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PseudoElement {
    Before,
    After,
    Placeholder,
    Marker,
    Selection,
    FirstLine,
    FirstLetter,
    Backdrop,
    FileSelectorButton,
    /// Any `::-webkit-…` name, lower-cased, without the colons.
    WebkitUnknown(String),
}

impl PseudoElement {
    fn from_name(name: &str) -> Option<Self> {
        let lc = name.to_ascii_lowercase();
        Some(match lc.as_str() {
            "before" => PseudoElement::Before,
            "after" => PseudoElement::After,
            "placeholder" => PseudoElement::Placeholder,
            "marker" => PseudoElement::Marker,
            "selection" => PseudoElement::Selection,
            "first-line" => PseudoElement::FirstLine,
            "first-letter" => PseudoElement::FirstLetter,
            "backdrop" => PseudoElement::Backdrop,
            "file-selector-button" => PseudoElement::FileSelectorButton,
            _ if lc.starts_with("-webkit-") => PseudoElement::WebkitUnknown(lc),
            _ => return None,
        })
    }
}

impl ToCss for PseudoElement {
    fn to_css<W>(&self, dest: &mut W) -> fmt::Result
    where
        W: fmt::Write,
    {
        dest.write_str("::")?;
        dest.write_str(match self {
            PseudoElement::Before => "before",
            PseudoElement::After => "after",
            PseudoElement::Placeholder => "placeholder",
            PseudoElement::Marker => "marker",
            PseudoElement::Selection => "selection",
            PseudoElement::FirstLine => "first-line",
            PseudoElement::FirstLetter => "first-letter",
            PseudoElement::Backdrop => "backdrop",
            PseudoElement::FileSelectorButton => "file-selector-button",
            PseudoElement::WebkitUnknown(name) => name,
        })
    }
}

impl selectors::parser::PseudoElement for PseudoElement {
    /// `::-webkit-scrollbar-thumb:hover`, `::before:hover`: user-action
    /// states after a pseudo-element are valid (Chrome keeps them).
    fn accepts_state_pseudo_classes(&self) -> bool {
        true
    }

    fn is_before_or_after(&self) -> bool {
        matches!(self, PseudoElement::Before | PseudoElement::After)
    }

    /// `::before::marker` is valid.
    fn valid_after_before_or_after(&self) -> bool {
        matches!(self, PseudoElement::Marker)
    }
}

impl selectors::SelectorImpl for MiniBrowserSelectorImpl {
    type ExtraMatchingData<'a> = ();
    type AttrValue = CssString;
    type Identifier = CssString;
    type LocalName = CssString;
    type NamespaceUrl = CssString;
    type NamespacePrefix = CssString;
    type BorrowedNamespaceUrl = str;
    type BorrowedLocalName = str;
    type NonTSPseudoClass = NonTSPseudoClass;
    type PseudoElement = PseudoElement;
}

/// Owning newtype for a comma-separated selector list (`h1, .title`). The
/// inner `selectors::SelectorList` is the parsed AST the matching crate
/// consumes; we wrap it so callers can store it in `Rule` / on JS bridge
/// objects without mentioning `MiniBrowserSelectorImpl` everywhere.
#[derive(Debug, Clone)]
pub struct Selector(pub SelectorsSelectorList<MiniBrowserSelectorImpl>);

impl Selector {
    /// Access the underlying `selectors::SelectorList`. Callers go through
    /// `selectors::matching::matches_selector_list` against this.
    pub fn list(&self) -> &SelectorsSelectorList<MiniBrowserSelectorImpl> {
        &self.0
    }

    /// Maximum specificity across the comma-separated branches. Used by the
    /// cascade in `style.rs` to break ties between equally-specified rules.
    pub fn specificity(&self) -> u32 {
        self.0
            .slice()
            .iter()
            .map(|s| s.specificity())
            .max()
            .unwrap_or(0)
    }
}

/// Most combinator levels one selector may stack through `:is()`/
/// `:where()`/`:not()` arguments (robustness, compat round 3 step 4a).
/// Matching an argument that has a combinator, inside a selector that has
/// one, walks the ancestors (or siblings) once per candidate of the outer
/// walk — the cost is DOM depth to the power of the levels (4 levels: 20 s
/// on a 200-deep chain). Two keep `:is(.a .b) .c` (real sites) at
/// O(depth²); a selector past the cap is invalid, the rule dropped.
pub const MAX_COMBINATOR_NESTING: usize = 2;

type InnerSelector = selectors::parser::Selector<MiniBrowserSelectorImpl>;

/// Combinator levels of `selector`: 1 if it has a combinator, plus the
/// deepest of its selector-list arguments. Recursion is bounded by
/// cssparser's block nesting limit (75).
fn combinator_nesting(selector: &InnerSelector) -> usize {
    let mut own = false;
    let mut inner = 0;
    for component in selector.iter_raw_match_order() {
        match component {
            Component::Combinator(combinator) if !combinator.is_pseudo_element() => own = true,
            Component::Is(list) | Component::Where(list) | Component::Negation(list) => {
                let deepest = list.slice().iter().map(combinator_nesting).max();
                inner = inner.max(deepest.unwrap_or(0));
            }
            _ => {}
        }
    }
    usize::from(own) + inner
}

/// Whether a forgiving `:is()`/`:where()` list (at any depth) dropped an
/// argument that didn't parse.
fn has_forgiven_argument(selector: &InnerSelector) -> bool {
    selector
        .iter_raw_match_order()
        .any(|component| match component {
            Component::Invalid(_) => true,
            Component::Is(list) | Component::Where(list) | Component::Negation(list) => {
                list.slice().iter().any(has_forgiven_argument)
            }
            _ => false,
        })
}

impl Selector {
    /// Within [`MAX_COMBINATOR_NESTING`] on every branch.
    pub(super) fn within_nesting_cap(&self) -> bool {
        self.0
            .slice()
            .iter()
            .all(|s| combinator_nesting(s) <= MAX_COMBINATOR_NESTING)
    }

    /// Whether some branch can match an element at all — false when every
    /// one ends in a pseudo-element (`a::before, .b::after`).
    pub fn can_match_elements(&self) -> bool {
        self.0.slice().iter().any(|s| !s.has_pseudo_element())
    }

    /// Whether any `:is()`/`:where()` argument was dropped by forgiving
    /// parsing — `@supports selector()` parses without forgiveness.
    pub fn has_forgiven_argument(&self) -> bool {
        self.0.slice().iter().any(has_forgiven_argument)
    }
}

// SelectorList is internally a tagged-pointer ThinArc; a deep structural
// `PartialEq` would have to walk the parsed components. For our purposes
// (test snapshotting), comparing two Stylesheets via `PartialEq` is no
// longer meaningful — the selector parsing tests now go through the
// matching surface instead. We still need *some* PartialEq to keep
// derived impls on Rule/Stylesheet alive, so we make it pointer-identity.
impl PartialEq for Selector {
    fn eq(&self, other: &Self) -> bool {
        self.0.thin_arc_heap_ptr() == other.0.thin_arc_heap_ptr()
    }
}

/// Selector parser callback for the `selectors` crate. It only needs to
/// know how to map our supported pseudo-class names — everything else
/// (combinators, attribute selectors, `:not()`, etc.) is handled by the
/// crate itself.
pub(super) struct SelectorParserImpl;

impl<'i> selectors::parser::Parser<'i> for SelectorParserImpl {
    type Impl = MiniBrowserSelectorImpl;
    type Error = selectors::parser::SelectorParseErrorKind;

    /// `:is()`/`:where()` — matched (and `:where`'s zero specificity) by
    /// the crate. Their argument lists are forgiving: an argument that
    /// doesn't parse is kept as never-matching instead of failing the rule.
    fn parse_is_and_where(&self) -> bool {
        true
    }

    fn parse_pseudo_element(
        &self,
        name: &str,
    ) -> Result<PseudoElement, CssParseError<Self::Error>> {
        PseudoElement::from_name(name).ok_or_else(|| {
            CssParseError::custom(
                selectors::parser::SelectorParseErrorKind::UnsupportedPseudoClassOrElement,
            )
        })
    }

    fn parse_non_ts_pseudo_class(
        &self,
        name: &str,
    ) -> Result<NonTSPseudoClass, CssParseError<Self::Error>> {
        // Case-insensitive dispatch over the non-tree-structural
        // pseudo-classes we wire through to `MatchingElement`. Anything
        // else parses to an unsupported-pseudo error which the surrounding
        // rule iterator (in `parse`) catches by skipping the rule.
        let lc = name.to_ascii_lowercase();
        match lc.as_str() {
            "hover" => Ok(NonTSPseudoClass::Hover),
            "focus" => Ok(NonTSPseudoClass::Focus),
            "active" => Ok(NonTSPseudoClass::Active),
            "link" => Ok(NonTSPseudoClass::Link),
            "visited" => Ok(NonTSPseudoClass::Visited),
            "focus-visible" => Ok(NonTSPseudoClass::FocusVisible),
            "focus-within" => Ok(NonTSPseudoClass::FocusWithin),
            "checked" => Ok(NonTSPseudoClass::Checked),
            "disabled" => Ok(NonTSPseudoClass::Disabled),
            "enabled" => Ok(NonTSPseudoClass::Enabled),
            _ => Err(CssParseError::custom(
                selectors::parser::SelectorParseErrorKind::UnsupportedPseudoClassOrElement,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use cssparser::ToCss;

    fn serialized(selector: &str) -> String {
        let parsed = super::super::parse_selector(selector).expect(selector);
        parsed.list().to_css_string()
    }

    #[test]
    fn pseudo_elements_and_is_where_serialize() {
        assert_eq!(serialized("a::before"), "a::before");
        assert_eq!(serialized("a:after"), "a::after");
        assert_eq!(
            serialized("::-webkit-scrollbar-thumb:hover"),
            "::-webkit-scrollbar-thumb:hover"
        );
        assert_eq!(
            serialized("input::-WEBKIT-Inner-Spin-Button"),
            "input::-webkit-inner-spin-button"
        );
        assert_eq!(serialized(":is(.a, :frob) p"), ":is(.a, :frob) p");
        assert_eq!(serialized(":where(#x)"), ":where(#x)");
    }
}
