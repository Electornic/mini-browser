// Selector glue for the `selectors` crate. Defines the
// `MiniBrowserSelectorImpl` flavour (pseudo-classes, namespace-free
// identifier type, no pseudo-elements), the owned `Selector` newtype
// wrapping the parsed `SelectorList`, and the `SelectorParserImpl`
// callback that maps recognised pseudo-class names. The rest of
// css/mod.rs treats this as an opaque toolkit — it imports `Selector`
// and (via `pub(super)`) `SelectorParserImpl` to feed into
// `SelectorList::parse`.

use std::borrow::Borrow;
use std::fmt;

use cssparser::{CowRcStr, ParseError as CssParseError, ToCss};
use precomputed_hash::PrecomputedHash;
use selectors::parser::SelectorList as SelectorsSelectorList;

/// Selector implementation flavour for the `selectors` crate. We don't need
/// namespaces, custom case sensitivity, pseudo-elements, or extra matching
/// data, so most associated types are stringy or unit.
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
/// pull live state from the `MatchingElement` impl in `style.rs`; `:link` /
/// `:visited` are wired to "anchor with href" / "never matches" the same way
/// the previous hand-rolled matcher did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonTSPseudoClass {
    Hover,
    Focus,
    Active,
    Link,
    Visited,
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
        })
    }
}

impl selectors::parser::NonTSPseudoClass for NonTSPseudoClass {
    type Impl = MiniBrowserSelectorImpl;

    fn is_active_or_hover(&self) -> bool {
        matches!(self, NonTSPseudoClass::Active | NonTSPseudoClass::Hover)
    }

    fn is_user_action_state(&self) -> bool {
        matches!(
            self,
            NonTSPseudoClass::Hover | NonTSPseudoClass::Active | NonTSPseudoClass::Focus
        )
    }
}

/// Uninhabited pseudo-element type: we don't model `::before`/`::after`/etc.
/// `void::Void` would do but it's not in std, so an empty enum suffices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PseudoElement {}

impl ToCss for PseudoElement {
    fn to_css<W>(&self, _dest: &mut W) -> fmt::Result
    where
        W: fmt::Write,
    {
        match *self {}
    }
}

impl selectors::parser::PseudoElement for PseudoElement {
    type Impl = MiniBrowserSelectorImpl;
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
    type Error = selectors::parser::SelectorParseErrorKind<'i>;

    fn parse_non_ts_pseudo_class(
        &self,
        location: cssparser::SourceLocation,
        name: CowRcStr<'i>,
    ) -> Result<NonTSPseudoClass, CssParseError<'i, Self::Error>> {
        // Case-insensitive dispatch over the five non-tree-structural
        // pseudo-classes we wire through to `MatchingElement`. Anything
        // else parses to an unsupported-pseudo error which the surrounding
        // rule iterator (in `parse`) catches by skipping the rule.
        let lc = name.as_ref().to_ascii_lowercase();
        match lc.as_str() {
            "hover" => Ok(NonTSPseudoClass::Hover),
            "focus" => Ok(NonTSPseudoClass::Focus),
            "active" => Ok(NonTSPseudoClass::Active),
            "link" => Ok(NonTSPseudoClass::Link),
            "visited" => Ok(NonTSPseudoClass::Visited),
            _ => Err(location.new_custom_error(
                selectors::parser::SelectorParseErrorKind::UnsupportedPseudoClassOrElement(name),
            )),
        }
    }
}
