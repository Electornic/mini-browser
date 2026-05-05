// CSS support is now driven by Servo's `selectors` crate for selector
// parsing + matching, and by `cssparser` for the rest of the rule body.
// The selector AST is no longer ours — `Selector` is a thin newtype around
// `selectors::SelectorList<MiniBrowserSelectorImpl>` so callers can hold
// owned, parsed selector lists without dragging the generic parameter
// through every file.
use std::borrow::Borrow;
use std::fmt;

use cssparser::{
    AtRuleParser as CssAtRuleParser, BasicParseErrorKind, CowRcStr, ParseError as CssParseError,
    Parser as CssParser, ParserInput, ParserState, QualifiedRuleParser as CssQualifiedRuleParser,
    StyleSheetParser, ToCss, Token,
};
use precomputed_hash::PrecomputedHash;
use selectors::parser::{ParseRelative, SelectorList as SelectorsSelectorList};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    /// Comma-separated selector list (e.g. `h1, .title`). Wrapped in a
    /// newtype so the `selectors`-crate generic doesn't leak into our
    /// public surface and so we can derive `PartialEq` for snapshot tests.
    pub selectors: Selector,
    pub declarations: Vec<Declaration>,
}

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
        self.0.slice().iter().map(|s| s.specificity()).max().unwrap_or(0)
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
struct SelectorParserImpl;

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

#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    pub name: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Keyword(String),
    Length(f32, Unit),
    Color(Color),
    // Unitless number — used for properties like `z-index`, `line-height`,
    // `opacity` where the value is a bare number rather than a length.
    Number(f32),
    // Functional `linear-gradient(...)` / `radial-gradient(...)` value, used
    // as a `background-image`. The shared `Gradient` carries the stops and
    // a `kind` enum that distinguishes the variant.
    Gradient(Gradient),
    // `box-shadow` value. MVP supports a single outset shadow with optional
    // blur, spread, and color — no inset, no comma-separated shadow lists.
    BoxShadow(BoxShadow),
    // `text-shadow` value. Same shape as box-shadow minus the spread.
    // The blur radius is parsed but not applied to glyph rendering yet;
    // sharpness vs softness on glyphs needs a real blur kernel.
    TextShadow(TextShadow),
    // `transform` value: an ordered list of transform functions applied
    // right-to-left to the box. The list grows by appending more variants
    // to `TransformOp` (translate first; scale/rotate land in later commits).
    TransformList(Vec<TransformOp>),
    // CSS Grid `grid-template-columns` / `grid-template-rows` value: an
    // ordered list of track sizes. `fr` is scoped to the track-list context
    // (it has no meaning as a stand-alone length), so it lives on `TrackSize`
    // instead of growing the global `Unit` enum.
    TrackList(Vec<TrackSize>),
    // CSS Grid `grid-column` / `grid-row` placement. Encodes a (start, end)
    // pair of grid lines or spans; the layout pass turns this into a
    // (cell_start, cell_end) cell range that may be auto-resolved against the
    // running grid cursor.
    GridPlacement(GridPlacement),
    // CSS Grid `grid-template-areas` value: a row-major map of cells to
    // optional area names. `None` represents the `.` token (empty cell).
    // Layout looks up an item's `grid-area` keyword in this map and uses
    // the bounding rectangle of matching cells as the item's placement.
    TemplateAreas(Vec<Vec<Option<String>>>),
    // `url("…")` reference, used as a `background-image` value. The string
    // is the raw CSS URL — the resolver in `resource::load_images` joins it
    // against the document's base URL when fetching, and the painter does
    // the same when looking the loaded pixels back up at paint time.
    ImageUrl(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridPlacement {
    pub start: GridLine,
    pub end: GridLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridLine {
    /// Auto: the layout pass either picks the next available line via
    /// auto-flow (when used as `start`) or treats the placement as span-1
    /// (when used as `end`).
    Auto,
    /// Explicit grid line, 1-based per CSS spec. Layout subtracts 1 to map
    /// to a 0-based cell index.
    Index(u32),
    /// `span <n>` form. Used for `start` ("auto-place but with span n") or
    /// `end` ("starts wherever, ends n cells later").
    Span(u32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrackSize {
    /// A fixed track sized via `<length>`. After style resolution this is
    /// always `Unit::Px` (em/rem already converted), but percent stays as-is
    /// to be resolved against the container at layout time.
    Length(f32, Unit),
    /// A flexible track sized via `<n>fr`. `1fr` carries weight 1.0, `2fr`
    /// carries 2.0, etc. Distribution against free space mirrors flex-grow.
    Fraction(f32),
    /// `auto` track — sizes to fit its widest item (the column's max-content
    /// natural width). Resolution requires a pre-pass to lay each item out
    /// without a track constraint and read its natural outer width.
    Auto,
}

/// Single function in a `transform: ...` list. Stored in source order so the
/// renderer can compose them right-to-left when building the affine matrix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransformOp {
    /// `translate(x[, y])` / `translateX(x)` / `translateY(y)`. Both axes
    /// are absolute pixel offsets — percent / em are deferred until the
    /// renderer carries the box's own size into the transform pass.
    Translate { x: f32, y: f32 },
    /// `scale(x[, y])` / `scaleX(x)` / `scaleY(y)`. Negative factors are
    /// allowed in spec land (mirroring); for the rasterizer's axis-aligned
    /// fast path here, scale-only matrices stay axis-aligned and the rect
    /// dimensions are multiplied through directly.
    Scale { x: f32, y: f32 },
    /// `rotate(<angle>)` in radians (the parser converts deg/rad/turn/grad
    /// into this canonical unit so the renderer never has to look at a
    /// CSS unit again). Rotation breaks axis-aligned rasterizing and
    /// triggers the slow inverse-pixel-sample path.
    Rotate(f32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxShadow {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur_radius: f32,
    pub spread_radius: f32,
    pub color: Color,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextShadow {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur_radius: f32,
    pub color: Color,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    pub kind: GradientKind,
    pub stops: Vec<ColorStop>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradientKind {
    Linear(GradientDirection),
    /// MVP radial: ellipse sized to farthest-corner, centered. No explicit
    /// shape/size/position support yet — extend this variant when adding it.
    Radial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradientDirection {
    ToTop,
    ToBottom,
    ToLeft,
    ToRight,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorStop {
    pub color: Color,
    /// 0.0–1.0 along the gradient. `None` means "let the renderer place it
    /// automatically by distributing evenly between defined neighbours."
    pub position: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Px,
    // Em and Rem are font-relative; resolution happens at style time once font-size
    // for the current node and the document root is known.
    Em,
    Rem,
    // Percent is containing-block-relative; resolution happens at layout time once
    // the parent's content box is known.
    Percent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const WHITE: Self = Self {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };

    pub const BLACK: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub position: usize,
    pub message: String,
}

impl ParseError {
    fn new(position: usize, message: impl Into<String>) -> Self {
        Self {
            position,
            message: message.into(),
        }
    }
}

pub fn parse(source: &str) -> Result<Stylesheet, ParseError> {
    let mut input = ParserInput::new(source);
    let mut parser = CssParser::new(&mut input);
    let mut handler = StylesheetHandler;
    let iter = StyleSheetParser::new(&mut parser, &mut handler);
    let mut rules = Vec::new();
    // Tolerant recovery: `StyleSheetParser` itself walks past a broken rule's
    // block before yielding the next item, so we just keep the successes and
    // discard the errors — same semantic as the previous `skip_to_end_of_block`.
    for rule in iter.flatten() {
        rules.push(rule);
    }
    Ok(Stylesheet { rules })
}

/// Parse a single complex selector (e.g. `div.card > a`). Used by JS-facing
/// `document.querySelector` to reuse the same selector grammar as the
/// stylesheet parser without going through a full stylesheet rule.
/// Trailing input after the selector is rejected so callers can't smuggle
/// declarations through the same entry point.
pub fn parse_selector(input: &str) -> Result<Selector, ParseError> {
    let mut parser_input = ParserInput::new(input);
    let mut parser = CssParser::new(&mut parser_input);
    let parsed = parser
        .parse_entirely(|input| {
            SelectorsSelectorList::parse(&SelectorParserImpl, input, ParseRelative::No)
        })
        .map_err(|err| convert_selector_error(err))?;
    Ok(Selector(parsed))
}

/// Convert a `selectors`-crate parse error into our `ParseError` shape.
/// We lose the structured `SelectorParseErrorKind` variant data because
/// it carries `Token<'_>` borrows, but the message string is enough for
/// the JS bridge's `SyntaxError` payload and the legacy stylesheet
/// recovery path drops parse failures wholesale anyway.
fn convert_selector_error<'i>(
    err: CssParseError<'i, selectors::parser::SelectorParseErrorKind<'i>>,
) -> ParseError {
    let position = err.location.line as usize;
    let message = format!("{:?}", err.kind);
    ParseError::new(position, message)
}

/// Returns the rgba color associated with a CSS named color keyword, if any.
/// Currently covers the HTML4 basic palette plus a handful of common extras and
/// `transparent`. Anything outside this set falls through to the generic keyword path.
pub fn named_color(name: &str) -> Option<Color> {
    let rgba = |r: u8, g: u8, b: u8, a: u8| Color { r, g, b, a };
    match name.to_ascii_lowercase().as_str() {
        // HTML4 basic 16.
        "black" => Some(rgba(0, 0, 0, 255)),
        "silver" => Some(rgba(192, 192, 192, 255)),
        "gray" | "grey" => Some(rgba(128, 128, 128, 255)),
        "white" => Some(rgba(255, 255, 255, 255)),
        "maroon" => Some(rgba(128, 0, 0, 255)),
        "red" => Some(rgba(255, 0, 0, 255)),
        "purple" => Some(rgba(128, 0, 128, 255)),
        "fuchsia" | "magenta" => Some(rgba(255, 0, 255, 255)),
        "green" => Some(rgba(0, 128, 0, 255)),
        "lime" => Some(rgba(0, 255, 0, 255)),
        "olive" => Some(rgba(128, 128, 0, 255)),
        "yellow" => Some(rgba(255, 255, 0, 255)),
        "navy" => Some(rgba(0, 0, 128, 255)),
        "blue" => Some(rgba(0, 0, 255, 255)),
        "teal" => Some(rgba(0, 128, 128, 255)),
        "aqua" | "cyan" => Some(rgba(0, 255, 255, 255)),
        // Common extras worth shipping early since toy pages reach for them.
        "orange" => Some(rgba(255, 165, 0, 255)),
        "pink" => Some(rgba(255, 192, 203, 255)),
        "brown" => Some(rgba(165, 42, 42, 255)),
        "transparent" => Some(rgba(0, 0, 0, 0)),
        _ => None,
    }
}

// =============================================================================
// cssparser glue: rule iteration via StyleSheetParser
// =============================================================================

struct StylesheetHandler;

/// Bundle a parsed selector list with the source location where it
/// started, so `parse_block` can build a `Rule` with the original prelude.
struct RulePrelude(Selector);

/// `cssparser` requires a custom error type; the toy parser converts everything
/// back into the legacy `ParseError` shape on the way out.
type CssError = ParseError;

impl<'i> CssQualifiedRuleParser<'i> for StylesheetHandler {
    type Prelude = RulePrelude;
    type QualifiedRule = Rule;
    type Error = CssError;

    fn parse_prelude<'t>(
        &mut self,
        input: &mut CssParser<'i, 't>,
    ) -> Result<Self::Prelude, CssParseError<'i, Self::Error>> {
        // Delegate the whole comma-separated selector grammar to the
        // selectors crate — it handles compound selectors, attribute
        // selectors, the descendant/child/sibling combinators,
        // pseudo-classes (via our `Parser` impl), `:is()`/`:where()`/
        // `:not()`, etc. We don't enable `parse_is_and_where` / `parse_has`
        // / `parse_part` / `parse_slotted` because we don't model the
        // matching machinery for them yet.
        match SelectorsSelectorList::parse(&SelectorParserImpl, input, ParseRelative::No) {
            Ok(list) => Ok(RulePrelude(Selector(list))),
            Err(err) => Err(input.new_custom_error(convert_selector_error(err))),
        }
    }

    fn parse_block<'t>(
        &mut self,
        prelude: Self::Prelude,
        _start: &ParserState,
        input: &mut CssParser<'i, 't>,
    ) -> Result<Self::QualifiedRule, CssParseError<'i, Self::Error>> {
        let declarations =
            parse_declaration_block(input).map_err(|err| input.new_custom_error(err))?;
        Ok(Rule {
            selectors: prelude.0,
            declarations,
        })
    }
}

impl<'i> CssAtRuleParser<'i> for StylesheetHandler {
    // At-rules (@media, @charset, @keyframes, etc.) are not modeled by the toy
    // parser. Reject every prelude so cssparser cleanly skips both inline and
    // block forms — same semantics as the previous `skip_at_rule` path.
    type Prelude = ();
    type AtRule = Rule;
    type Error = CssError;
}

// =============================================================================
// Declaration block parsing
// =============================================================================

/// Iterate the `{ ... }` body and collect declarations. Errors on individual
/// declarations are tolerated: the offending entry is dropped and the next one
/// continues. `RuleBodyParser` already scans past the next semicolon on error
/// so the tolerant behaviour matches the original parser.
fn parse_declaration_block<'i, 't>(
    input: &mut CssParser<'i, 't>,
) -> Result<Vec<Declaration>, ParseError> {
    let mut declarations = Vec::new();
    let mut handler = DeclHandler;
    let iter = cssparser::RuleBodyParser::new(input, &mut handler);
    for decls in iter.flatten() {
        declarations.extend(decls);
    }
    Ok(declarations)
}

struct DeclHandler;

impl<'i> cssparser::DeclarationParser<'i> for DeclHandler {
    type Declaration = Vec<Declaration>;
    type Error = CssError;

    fn parse_value<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut CssParser<'i, 't>,
        _decl_start: &ParserState,
    ) -> Result<Self::Declaration, CssParseError<'i, Self::Error>> {
        // Dispatch by property name to mirror the old `parse_declaration`. The
        // value-shape helpers each consume their delimited slice end-to-end so
        // `parse_value` itself just routes by name.
        let name = name.to_string();
        let result = parse_declaration_value(&name, input);
        match result {
            Ok(decls) => Ok(decls),
            Err(err) => Err(input.new_custom_error(err)),
        }
    }
}

impl<'i> cssparser::AtRuleParser<'i> for DeclHandler {
    type Prelude = ();
    type AtRule = Vec<Declaration>;
    type Error = CssError;
}

impl<'i> cssparser::QualifiedRuleParser<'i> for DeclHandler {
    type Prelude = ();
    type QualifiedRule = Vec<Declaration>;
    type Error = CssError;
}

impl<'i> cssparser::RuleBodyItemParser<'i, Vec<Declaration>, CssError> for DeclHandler {
    fn parse_declarations(&self) -> bool {
        true
    }

    fn parse_qualified(&self) -> bool {
        false
    }
}

/// Per-property dispatch: returns the longhand declarations contributed by
/// a single source declaration. Shorthands like `border-radius` and `flex`
/// expand to several entries.
fn parse_declaration_value<'i, 't>(
    name: &str,
    input: &mut CssParser<'i, 't>,
) -> Result<Vec<Declaration>, ParseError> {
    if name == "border-radius" {
        return parse_border_radius_shorthand(name, input);
    }
    if name == "box-shadow" {
        let value = parse_box_shadow_value(input)?;
        return Ok(vec![Declaration {
            name: name.to_string(),
            value,
        }]);
    }
    if name == "text-shadow" {
        let value = parse_text_shadow_value(input)?;
        return Ok(vec![Declaration {
            name: name.to_string(),
            value,
        }]);
    }
    if name == "transform" {
        let value = parse_transform_value(input)?;
        return Ok(vec![Declaration {
            name: name.to_string(),
            value,
        }]);
    }
    if name == "flex" {
        return parse_flex_shorthand(input);
    }
    if name == "grid-template-columns" || name == "grid-template-rows" {
        let value = parse_grid_track_list(input)?;
        return Ok(vec![Declaration {
            name: name.to_string(),
            value,
        }]);
    }
    if name == "grid-column" || name == "grid-row" {
        let value = parse_grid_placement(input)?;
        return Ok(vec![Declaration {
            name: name.to_string(),
            value,
        }]);
    }
    if name == "grid-template-areas" {
        let value = parse_grid_template_areas(input)?;
        return Ok(vec![Declaration {
            name: name.to_string(),
            value,
        }]);
    }

    let value = parse_value(input)?;
    Ok(vec![Declaration {
        name: name.to_string(),
        value,
    }])
}

// =============================================================================
// Value parsing (the bulk of the per-property quirks)
// =============================================================================

/// Generic value parser used as the fallback dispatch for any property that
/// doesn't have a custom shape. Recognises keywords, numbers/lengths, hex
/// colors, named colors, and the legacy `rgb()` / `rgba()` / `linear-gradient()`
/// / `radial-gradient()` / `url()` functions.
fn parse_value<'i, 't>(input: &mut CssParser<'i, 't>) -> Result<Value, ParseError> {
    input.skip_whitespace();
    let position = input.position();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(position.byte_index(), err))?
        .clone();

    let value = match token {
        Token::Hash(hex) | Token::IDHash(hex) => parse_hex_color_str(hex.as_ref(), input)?,
        Token::Number { value, .. } => Value::Number(value),
        Token::Percentage { unit_value, .. } => Value::Length(unit_value * 100.0, Unit::Percent),
        Token::Dimension { value, unit, .. } => length_with_unit(value, unit.as_ref()),
        Token::Ident(ident) => {
            let raw = ident.to_string();
            if let Some(color) = named_color(&raw) {
                Value::Color(color)
            } else {
                Value::Keyword(raw)
            }
        }
        Token::Function(name) => parse_function(name.as_ref(), input)?,
        // Unquoted URL: cssparser turns the entire `url(  /a/b  )` into a single
        // `UnquotedUrl` token (already trimmed of inner whitespace), so the
        // outer `Function("url")` route only fires on the quoted form.
        Token::UnquotedUrl(url) => Value::ImageUrl(url.to_string()),
        Token::QuotedString(s) => Value::Keyword(s.to_string()),
        other => {
            return Err(token_error(input, &other, "unexpected token in value"));
        }
    };

    Ok(value)
}

fn length_with_unit(value: f32, unit: &str) -> Value {
    match unit {
        "px" => Value::Length(value, Unit::Px),
        "em" => Value::Length(value, Unit::Em),
        "rem" => Value::Length(value, Unit::Rem),
        // Unsupported dimensions (e.g. `12pt`) fall back to a keyword that mirrors the
        // original tokens so callers can still distinguish them at the cascade layer.
        other => Value::Keyword(format!("{value}{other}")),
    }
}

fn parse_hex_color_str<'i, 't>(
    hex: &str,
    input: &CssParser<'i, 't>,
) -> Result<Value, ParseError> {
    let (r, g, b) = match hex.len() {
        3 => {
            let chars: Vec<char> = hex.chars().collect();
            (
                expand_hex(chars[0])?,
                expand_hex(chars[1])?,
                expand_hex(chars[2])?,
            )
        }
        6 => (
            parse_hex_pair(&hex[0..2])?,
            parse_hex_pair(&hex[2..4])?,
            parse_hex_pair(&hex[4..6])?,
        ),
        _ => {
            return Err(ParseError::new(
                input.position().byte_index(),
                "hex colors must use either 3 or 6 digits",
            ));
        }
    };
    Ok(Value::Color(Color { r, g, b, a: 255 }))
}

fn parse_hex_pair(pair: &str) -> Result<u8, ParseError> {
    u8::from_str_radix(pair, 16)
        .map_err(|_| ParseError::new(0, format!("invalid hex color pair '{pair}'")))
}

fn expand_hex(ch: char) -> Result<u8, ParseError> {
    let mut pair = String::with_capacity(2);
    pair.push(ch);
    pair.push(ch);
    parse_hex_pair(&pair)
}

/// Drive a function-call value (`rgb()`, `linear-gradient()`, etc). The caller
/// has already consumed the leading `Function(name)` token, so we open a nested
/// block and route by name.
fn parse_function<'i, 't>(
    name: &str,
    input: &mut CssParser<'i, 't>,
) -> Result<Value, ParseError> {
    let lower = name.to_ascii_lowercase();
    input
        .parse_nested_block(|inner| {
            let result: Result<Value, ParseError> = match lower.as_str() {
                "rgb" => parse_rgb_function(inner, false),
                "rgba" => parse_rgb_function(inner, true),
                "linear-gradient" => parse_linear_gradient(inner),
                "radial-gradient" => parse_radial_gradient(inner),
                "url" => parse_url_function(inner),
                other => Err(ParseError::new(
                    inner.position().byte_index(),
                    format!("unsupported function '{other}'"),
                )),
            };
            result.map_err(|err| inner.new_custom_error(err))
        })
        .map_err(|err| convert_error(err))
}

fn parse_rgb_function<'i, 't>(
    input: &mut CssParser<'i, 't>,
    has_alpha: bool,
) -> Result<Value, ParseError> {
    let r = parse_color_byte(input)?;
    expect_token_comma(input)?;
    let g = parse_color_byte(input)?;
    expect_token_comma(input)?;
    let b = parse_color_byte(input)?;
    let a = if has_alpha {
        expect_token_comma(input)?;
        let alpha = parse_unsigned_number(input)?;
        (alpha.clamp(0.0, 1.0) * 255.0).round() as u8
    } else {
        255
    };
    input.skip_whitespace();
    Ok(Value::Color(Color { r, g, b, a }))
}

fn parse_color_byte<'i, 't>(input: &mut CssParser<'i, 't>) -> Result<u8, ParseError> {
    let value = parse_unsigned_number(input)?;
    Ok(value.clamp(0.0, 255.0).round() as u8)
}

fn parse_unsigned_number<'i, 't>(input: &mut CssParser<'i, 't>) -> Result<f32, ParseError> {
    input.skip_whitespace();
    let pos = input.position().byte_index();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(pos, err))?
        .clone();
    match token {
        Token::Number { value, .. } => Ok(value),
        other => Err(token_error(input, &other, "invalid numeric component")),
    }
}

fn expect_token_comma<'i, 't>(input: &mut CssParser<'i, 't>) -> Result<(), ParseError> {
    input.skip_whitespace();
    let pos = input.position().byte_index();
    input
        .expect_comma()
        .map_err(|err| convert_basic_error_at(pos, err))
}

// -----------------------------------------------------------------------------
// linear-gradient / radial-gradient
// -----------------------------------------------------------------------------

fn parse_linear_gradient<'i, 't>(input: &mut CssParser<'i, 't>) -> Result<Value, ParseError> {
    input.skip_whitespace();

    // Look for an optional `to <side>` direction prefix. We commit to the
    // direction parse only if the first token looks like the `to` keyword;
    // otherwise back out and treat the input as a stops-only gradient.
    let direction = {
        let saved = input.state();
        let probe = input.next().ok().cloned();
        match probe {
            Some(Token::Ident(ref ident)) if ident.eq_ignore_ascii_case("to") => {
                input.skip_whitespace();
                let side_pos = input.position().byte_index();
                let side_tok = input
                    .next()
                    .map_err(|err| convert_basic_error_at(side_pos, err))?
                    .clone();
                let dir = match side_tok {
                    Token::Ident(side) => match side.as_ref() {
                        "top" => GradientDirection::ToTop,
                        "bottom" => GradientDirection::ToBottom,
                        "left" => GradientDirection::ToLeft,
                        "right" => GradientDirection::ToRight,
                        other => {
                            return Err(ParseError::new(
                                side_pos,
                                format!("unsupported gradient direction 'to {other}'"),
                            ));
                        }
                    },
                    other => {
                        return Err(token_error(input, &other, "expected gradient direction side"));
                    }
                };
                input.skip_whitespace();
                let comma_pos = input.position().byte_index();
                input
                    .expect_comma()
                    .map_err(|err| convert_basic_error_at(comma_pos, err))?;
                dir
            }
            _ => {
                input.reset(&saved);
                GradientDirection::ToBottom
            }
        }
    };

    let stops = parse_gradient_stops(input, "linear-gradient")?;
    Ok(Value::Gradient(Gradient {
        kind: GradientKind::Linear(direction),
        stops,
    }))
}

fn parse_radial_gradient<'i, 't>(input: &mut CssParser<'i, 't>) -> Result<Value, ParseError> {
    input.skip_whitespace();
    let stops = parse_gradient_stops(input, "radial-gradient")?;
    Ok(Value::Gradient(Gradient {
        kind: GradientKind::Radial,
        stops,
    }))
}

fn parse_gradient_stops<'i, 't>(
    input: &mut CssParser<'i, 't>,
    label: &str,
) -> Result<Vec<ColorStop>, ParseError> {
    let mut stops = Vec::new();
    loop {
        input.skip_whitespace();
        stops.push(parse_color_stop(input)?);
        input.skip_whitespace();
        // We're inside `parse_nested_block`, so `next()` returns `Err` once we
        // hit the closing `)`. A comma means another stop follows; anything
        // else (including end-of-block) ends the list.
        let probe = input.state();
        match input.next() {
            Ok(Token::Comma) => continue,
            Ok(other) => {
                let other = other.clone();
                return Err(token_error(input, &other, &format!("expected ',' or ')' in {label}")));
            }
            Err(_) => {
                input.reset(&probe);
                break;
            }
        }
    }
    if stops.len() < 2 {
        return Err(ParseError::new(
            input.position().byte_index(),
            format!("{label} requires at least two color stops"),
        ));
    }
    Ok(stops)
}

fn parse_color_stop<'i, 't>(input: &mut CssParser<'i, 't>) -> Result<ColorStop, ParseError> {
    let color = match parse_value(input)? {
        Value::Color(color) => color,
        other => {
            return Err(ParseError::new(
                input.position().byte_index(),
                format!("expected a color in gradient stop, got {other:?}"),
            ));
        }
    };
    input.skip_whitespace();
    let probe = input.state();
    let position = match input.next().ok().cloned() {
        Some(Token::Percentage { unit_value, .. }) => Some(unit_value.clamp(0.0, 1.0)),
        _ => {
            input.reset(&probe);
            None
        }
    };
    Ok(ColorStop { color, position })
}

// -----------------------------------------------------------------------------
// url(...)
// -----------------------------------------------------------------------------

fn parse_url_function<'i, 't>(input: &mut CssParser<'i, 't>) -> Result<Value, ParseError> {
    // Inside the nested block opened by `Function("url")`. The token for the URL
    // is either an `UnquotedUrl` (cssparser already trimmed surrounding
    // whitespace) or a `QuotedString`. Anything else is a parse error.
    input.skip_whitespace();
    let pos = input.position().byte_index();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(pos, err))?
        .clone();
    let url = match token {
        Token::UnquotedUrl(url) => url.to_string(),
        Token::QuotedString(url) => url.to_string(),
        other => {
            return Err(token_error(input, &other, "expected URL token"));
        }
    };
    input.skip_whitespace();
    Ok(Value::ImageUrl(url))
}

// -----------------------------------------------------------------------------
// length / number helpers (with sign + unit)
// -----------------------------------------------------------------------------

/// Parse a single number-or-length token where a leading sign is allowed. Used
/// by transform / shadow / flex / border-radius — anywhere we want to accept
/// `-10px` or `1.5` without the generic `parse_value` machinery.
fn parse_length_or_number<'i, 't>(
    input: &mut CssParser<'i, 't>,
) -> Result<Value, ParseError> {
    input.skip_whitespace();
    let pos = input.position().byte_index();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(pos, err))?
        .clone();
    match token {
        Token::Number { value, .. } => Ok(Value::Number(value)),
        Token::Percentage { unit_value, .. } => Ok(Value::Length(unit_value * 100.0, Unit::Percent)),
        Token::Dimension { value, unit, .. } => Ok(length_with_unit(value, unit.as_ref())),
        other => Err(token_error(input, &other, "expected a length or number")),
    }
}

fn parse_length_token<'i, 't>(input: &mut CssParser<'i, 't>) -> Result<f32, ParseError> {
    match parse_length_or_number(input)? {
        Value::Length(v, _) => Ok(v),
        Value::Number(v) => Ok(v),
        other => Err(ParseError::new(
            input.position().byte_index(),
            format!("expected a length token, got {other:?}"),
        )),
    }
}

/// `true` if the next token can begin a numeric value (number, dimension,
/// percentage). Used by shadow / flex parsers that greedily consume a variable
/// number of leading lengths.
fn peek_starts_length<'i, 't>(input: &mut CssParser<'i, 't>) -> bool {
    let saved = input.state();
    let result = matches!(
        input.next(),
        Ok(Token::Number { .. }) | Ok(Token::Dimension { .. }) | Ok(Token::Percentage { .. })
    );
    input.reset(&saved);
    result
}

// -----------------------------------------------------------------------------
// border-radius shorthand
// -----------------------------------------------------------------------------

fn parse_border_radius_shorthand<'i, 't>(
    name: &str,
    input: &mut CssParser<'i, 't>,
) -> Result<Vec<Declaration>, ParseError> {
    let _ = name; // shorthand always expands to fixed longhand names
    let mut lengths = Vec::new();
    loop {
        input.skip_whitespace();
        if !peek_starts_length(input) {
            break;
        }
        lengths.push(parse_length_or_number(input)?);
        if lengths.len() == 4 {
            break;
        }
    }

    if lengths.is_empty() {
        return Err(ParseError::new(
            input.position().byte_index(),
            "border-radius requires at least one length",
        ));
    }

    let (tl, tr, br, bl) = match lengths.as_slice() {
        [v] => (v.clone(), v.clone(), v.clone(), v.clone()),
        [tl_br, tr_bl] => (tl_br.clone(), tr_bl.clone(), tl_br.clone(), tr_bl.clone()),
        [tl, tr_bl, br] => (tl.clone(), tr_bl.clone(), br.clone(), tr_bl.clone()),
        [tl, tr, br, bl] => (tl.clone(), tr.clone(), br.clone(), bl.clone()),
        _ => unreachable!(),
    };

    Ok(vec![
        Declaration {
            name: "border-top-left-radius".into(),
            value: tl,
        },
        Declaration {
            name: "border-top-right-radius".into(),
            value: tr,
        },
        Declaration {
            name: "border-bottom-right-radius".into(),
            value: br,
        },
        Declaration {
            name: "border-bottom-left-radius".into(),
            value: bl,
        },
    ])
}

// -----------------------------------------------------------------------------
// flex shorthand
// -----------------------------------------------------------------------------

fn parse_flex_shorthand<'i, 't>(
    input: &mut CssParser<'i, 't>,
) -> Result<Vec<Declaration>, ParseError> {
    // Same grammar as the original parser — see the long comment in the prior
    // implementation. We greedily consume up to three numeric tokens and stop
    // when we see a length (which becomes the basis) or run out.
    let mut grow: Option<f32> = None;
    let mut shrink: Option<f32> = None;
    let mut basis: Option<Value> = None;

    for _slot in 0..3 {
        input.skip_whitespace();
        if !peek_starts_length(input) {
            break;
        }
        match parse_length_or_number(input)? {
            Value::Number(n) => {
                if grow.is_none() {
                    grow = Some(n);
                } else if shrink.is_none() {
                    shrink = Some(n);
                } else {
                    return Err(ParseError::new(
                        input.position().byte_index(),
                        "flex shorthand: third value must be a length",
                    ));
                }
            }
            length @ Value::Length(_, _) => {
                basis = Some(length);
                break;
            }
            _ => break,
        }
    }

    if grow.is_none() && basis.is_none() {
        return Err(ParseError::new(
            input.position().byte_index(),
            "flex shorthand requires at least one number or length",
        ));
    }

    let grow_value = grow.unwrap_or(1.0);
    let shrink_value = shrink.unwrap_or(1.0);
    let mut decls = vec![
        Declaration {
            name: "flex-grow".into(),
            value: Value::Number(grow_value),
        },
        Declaration {
            name: "flex-shrink".into(),
            value: Value::Number(shrink_value),
        },
    ];
    if let Some(basis_value) = basis {
        decls.push(Declaration {
            name: "flex-basis".into(),
            value: basis_value,
        });
    }
    Ok(decls)
}

// -----------------------------------------------------------------------------
// transform list
// -----------------------------------------------------------------------------

fn parse_transform_value<'i, 't>(
    input: &mut CssParser<'i, 't>,
) -> Result<Value, ParseError> {
    let mut ops = Vec::new();
    loop {
        input.skip_whitespace();
        // Stop at end-of-input (we're in a delimited declaration slice, so
        // `next()` returning Err means there's nothing more).
        let probe = input.state();
        let func = match input.next() {
            Ok(Token::Function(name)) => name.to_string(),
            Ok(_) => {
                input.reset(&probe);
                break;
            }
            Err(_) => {
                input.reset(&probe);
                break;
            }
        };
        let op = input
            .parse_nested_block(|inner| {
                let result: Result<TransformOp, ParseError> = parse_transform_op(&func, inner);
                result.map_err(|err| inner.new_custom_error(err))
            })
            .map_err(|err| convert_error(err))?;
        ops.push(op);
    }
    if ops.is_empty() {
        return Err(ParseError::new(
            input.position().byte_index(),
            "transform requires at least one function",
        ));
    }
    Ok(Value::TransformList(ops))
}

fn parse_transform_op<'i, 't>(
    name: &str,
    input: &mut CssParser<'i, 't>,
) -> Result<TransformOp, ParseError> {
    match name {
        "translate" => {
            input.skip_whitespace();
            let x = parse_length_token(input)?;
            input.skip_whitespace();
            let y = if input.try_parse(|i| i.expect_comma()).is_ok() {
                input.skip_whitespace();
                let value = parse_length_token(input)?;
                input.skip_whitespace();
                value
            } else {
                0.0
            };
            Ok(TransformOp::Translate { x, y })
        }
        "translateX" => {
            input.skip_whitespace();
            let x = parse_length_token(input)?;
            input.skip_whitespace();
            Ok(TransformOp::Translate { x, y: 0.0 })
        }
        "translateY" => {
            input.skip_whitespace();
            let y = parse_length_token(input)?;
            input.skip_whitespace();
            Ok(TransformOp::Translate { x: 0.0, y })
        }
        "scale" => {
            input.skip_whitespace();
            let x = parse_length_token(input)?;
            input.skip_whitespace();
            let y = if input.try_parse(|i| i.expect_comma()).is_ok() {
                input.skip_whitespace();
                let value = parse_length_token(input)?;
                input.skip_whitespace();
                value
            } else {
                x
            };
            Ok(TransformOp::Scale { x, y })
        }
        "scaleX" => {
            input.skip_whitespace();
            let x = parse_length_token(input)?;
            input.skip_whitespace();
            Ok(TransformOp::Scale { x, y: 1.0 })
        }
        "scaleY" => {
            input.skip_whitespace();
            let y = parse_length_token(input)?;
            input.skip_whitespace();
            Ok(TransformOp::Scale { x: 1.0, y })
        }
        "rotate" => {
            input.skip_whitespace();
            let theta = parse_angle_token(input)?;
            input.skip_whitespace();
            Ok(TransformOp::Rotate(theta))
        }
        other => Err(ParseError::new(
            input.position().byte_index(),
            format!("unsupported transform function '{other}'"),
        )),
    }
}

fn parse_angle_token<'i, 't>(input: &mut CssParser<'i, 't>) -> Result<f32, ParseError> {
    input.skip_whitespace();
    let pos = input.position().byte_index();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(pos, err))?
        .clone();
    let (value, unit): (f32, String) = match token {
        Token::Number { value, .. } => (value, String::new()),
        Token::Dimension { value, unit, .. } => (value, unit.to_string()),
        other => return Err(token_error(input, &other, "invalid angle")),
    };
    let radians = match unit.as_str() {
        "deg" => value * std::f32::consts::PI / 180.0,
        "rad" => value,
        "turn" => value * std::f32::consts::TAU,
        "grad" => value * std::f32::consts::PI / 200.0,
        "" if value == 0.0 => 0.0,
        other => {
            return Err(ParseError::new(
                pos,
                format!("unsupported angle unit '{other}'"),
            ));
        }
    };
    Ok(radians)
}

// -----------------------------------------------------------------------------
// box-shadow / text-shadow
// -----------------------------------------------------------------------------

fn parse_box_shadow_value<'i, 't>(
    input: &mut CssParser<'i, 't>,
) -> Result<Value, ParseError> {
    let offset_x = parse_length_token(input)?;
    input.skip_whitespace();
    let offset_y = parse_length_token(input)?;
    input.skip_whitespace();

    let mut blur_radius = 0.0;
    let mut spread_radius = 0.0;
    for slot in 0..2 {
        if !peek_starts_length(input) {
            break;
        }
        let value = parse_length_token(input)?;
        if slot == 0 {
            blur_radius = value.max(0.0);
        } else {
            spread_radius = value;
        }
        input.skip_whitespace();
    }

    let color = if input.is_exhausted() {
        Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        }
    } else {
        match parse_value(input)? {
            Value::Color(color) => color,
            other => {
                return Err(ParseError::new(
                    input.position().byte_index(),
                    format!("expected color in box-shadow, got {other:?}"),
                ));
            }
        }
    };

    Ok(Value::BoxShadow(BoxShadow {
        offset_x,
        offset_y,
        blur_radius,
        spread_radius,
        color,
    }))
}

fn parse_text_shadow_value<'i, 't>(
    input: &mut CssParser<'i, 't>,
) -> Result<Value, ParseError> {
    let offset_x = parse_length_token(input)?;
    input.skip_whitespace();
    let offset_y = parse_length_token(input)?;
    input.skip_whitespace();

    let blur_radius = if peek_starts_length(input) {
        let value = parse_length_token(input)?.max(0.0);
        input.skip_whitespace();
        value
    } else {
        0.0
    };

    let color = if input.is_exhausted() {
        Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        }
    } else {
        match parse_value(input)? {
            Value::Color(color) => color,
            other => {
                return Err(ParseError::new(
                    input.position().byte_index(),
                    format!("expected color in text-shadow, got {other:?}"),
                ));
            }
        }
    };

    Ok(Value::TextShadow(TextShadow {
        offset_x,
        offset_y,
        blur_radius,
        color,
    }))
}

// -----------------------------------------------------------------------------
// grid-* values
// -----------------------------------------------------------------------------

fn parse_grid_template_areas<'i, 't>(
    input: &mut CssParser<'i, 't>,
) -> Result<Value, ParseError> {
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    loop {
        input.skip_whitespace();
        let probe = input.state();
        let body = match input.next() {
            Ok(Token::QuotedString(s)) => s.to_string(),
            _ => {
                input.reset(&probe);
                break;
            }
        };
        let cells = body
            .split_whitespace()
            .map(|tok| {
                if tok == "." {
                    None
                } else {
                    Some(tok.to_string())
                }
            })
            .collect::<Vec<_>>();
        if !cells.is_empty() {
            rows.push(cells);
        }
    }
    if rows.is_empty() {
        return Err(ParseError::new(
            input.position().byte_index(),
            "grid-template-areas requires at least one row string",
        ));
    }
    Ok(Value::TemplateAreas(rows))
}

fn parse_grid_placement<'i, 't>(
    input: &mut CssParser<'i, 't>,
) -> Result<Value, ParseError> {
    input.skip_whitespace();
    let start = parse_grid_line(input)?;
    input.skip_whitespace();
    let end = if input.try_parse(|i| i.expect_delim('/')).is_ok() {
        input.skip_whitespace();
        parse_grid_line(input)?
    } else {
        GridLine::Auto
    };
    Ok(Value::GridPlacement(GridPlacement { start, end }))
}

fn parse_grid_line<'i, 't>(input: &mut CssParser<'i, 't>) -> Result<GridLine, ParseError> {
    let pos = input.position().byte_index();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(pos, err))?
        .clone();
    match token {
        Token::Ident(name) => match name.as_ref() {
            "auto" => Ok(GridLine::Auto),
            "span" => {
                input.skip_whitespace();
                let n = parse_grid_line_integer(input)?;
                if n == 0 {
                    return Err(ParseError::new(pos, "span must be >= 1"));
                }
                Ok(GridLine::Span(n))
            }
            other => Err(ParseError::new(
                pos,
                format!("unsupported grid-line keyword '{other}'"),
            )),
        },
        Token::Number {
            int_value: Some(int_value),
            value,
            ..
        } => {
            if int_value <= 0 {
                return Err(ParseError::new(pos, "grid line must be >= 1"));
            }
            let _ = value;
            Ok(GridLine::Index(int_value as u32))
        }
        Token::Number { value, .. } => {
            if value <= 0.0 {
                return Err(ParseError::new(pos, "grid line must be >= 1"));
            }
            Ok(GridLine::Index(value as u32))
        }
        other => Err(token_error(input, &other, "expected grid line value")),
    }
}

fn parse_grid_line_integer<'i, 't>(
    input: &mut CssParser<'i, 't>,
) -> Result<u32, ParseError> {
    let pos = input.position().byte_index();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(pos, err))?
        .clone();
    match token {
        Token::Number {
            int_value: Some(n),
            ..
        } if n >= 0 => Ok(n as u32),
        Token::Number { value, .. } if value >= 0.0 => Ok(value as u32),
        other => Err(token_error(input, &other, "invalid grid line integer")),
    }
}

fn parse_grid_track_list<'i, 't>(
    input: &mut CssParser<'i, 't>,
) -> Result<Value, ParseError> {
    let mut tracks = Vec::new();
    loop {
        input.skip_whitespace();
        if input.is_exhausted() {
            break;
        }
        let track = parse_grid_track_size(input)?;
        tracks.push(track);
    }
    if tracks.is_empty() {
        return Err(ParseError::new(
            input.position().byte_index(),
            "grid track list requires at least one track size",
        ));
    }
    Ok(Value::TrackList(tracks))
}

fn parse_grid_track_size<'i, 't>(
    input: &mut CssParser<'i, 't>,
) -> Result<TrackSize, ParseError> {
    let pos = input.position().byte_index();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(pos, err))?
        .clone();
    match token {
        Token::Ident(name) => match name.as_ref() {
            "auto" => Ok(TrackSize::Auto),
            other => Err(ParseError::new(
                pos,
                format!("unsupported grid track keyword '{other}'"),
            )),
        },
        Token::Percentage { unit_value, .. } => {
            Ok(TrackSize::Length(unit_value * 100.0, Unit::Percent))
        }
        Token::Dimension { value, unit, .. } => match unit.as_ref() {
            "fr" => Ok(TrackSize::Fraction(value)),
            "px" => Ok(TrackSize::Length(value, Unit::Px)),
            "em" => Ok(TrackSize::Length(value, Unit::Em)),
            "rem" => Ok(TrackSize::Length(value, Unit::Rem)),
            other => Err(ParseError::new(
                pos,
                format!("unsupported grid track unit '{other}'"),
            )),
        },
        Token::Number { .. } => Err(ParseError::new(
            pos,
            "grid track size requires a unit (px/em/rem/% or fr)",
        )),
        other => Err(token_error(input, &other, "invalid grid track size")),
    }
}

// =============================================================================
// Error conversion helpers
// =============================================================================

fn convert_error<'i>(err: CssParseError<'i, CssError>) -> ParseError {
    let position = err.location.line as usize; // best-effort fallback
    match err.kind {
        cssparser::ParseErrorKind::Custom(custom) => custom,
        cssparser::ParseErrorKind::Basic(basic) => match basic {
            BasicParseErrorKind::EndOfInput => ParseError::new(position, "unexpected end of input"),
            BasicParseErrorKind::UnexpectedToken(token) => {
                ParseError::new(position, format!("unexpected token: {token:?}"))
            }
            BasicParseErrorKind::AtRuleInvalid(name) => {
                ParseError::new(position, format!("invalid at-rule '@{name}'"))
            }
            BasicParseErrorKind::AtRuleBodyInvalid => {
                ParseError::new(position, "invalid at-rule body")
            }
            BasicParseErrorKind::QualifiedRuleInvalid => {
                ParseError::new(position, "invalid qualified rule")
            }
        },
    }
}

fn convert_basic_error_at<'i>(
    position: usize,
    err: cssparser::BasicParseError<'i>,
) -> ParseError {
    let message = match err.kind {
        BasicParseErrorKind::EndOfInput => "unexpected end of input".to_string(),
        BasicParseErrorKind::UnexpectedToken(token) => format!("unexpected token: {token:?}"),
        BasicParseErrorKind::AtRuleInvalid(name) => format!("invalid at-rule '@{name}'"),
        BasicParseErrorKind::AtRuleBodyInvalid => "invalid at-rule body".to_string(),
        BasicParseErrorKind::QualifiedRuleInvalid => "invalid qualified rule".to_string(),
    };
    ParseError::new(position, message)
}

fn token_error<'i, 't>(
    input: &CssParser<'i, 't>,
    token: &Token<'_>,
    message: &str,
) -> ParseError {
    ParseError::new(
        input.position().byte_index(),
        format!("{message}: got {token:?}"),
    )
}


#[cfg(test)]
mod tests {
    use super::{
        BoxShadow, Color, GradientDirection, GradientKind, GridLine, TextShadow, TrackSize,
        TransformOp, Unit, Value, parse,
    };

    // The selector AST is now opaque (owned by the `selectors` crate),
    // so parser tests assert the public surface (rule count + declarations
    // + selector list length) rather than diving into specific selector
    // components. Behavioural coverage of combinators / pseudo-classes /
    // compound selectors lives in `style::tests`, where it can observe the
    // cascade output the parsing actually drives.
    #[test]
    fn parses_multiple_rules_and_selectors() {
        let stylesheet = parse(
            r#"
                h1, .title {
                    color: #ff0000;
                    font-size: 24px;
                }

                #app {
                    display: block;
                }
            "#,
        )
        .unwrap();

        assert_eq!(stylesheet.rules.len(), 2);
        // First rule: a comma-separated list (`h1, .title`) parses into
        // two branches inside the `SelectorList`.
        assert_eq!(stylesheet.rules[0].selectors.list().len(), 2);
        assert_eq!(
            stylesheet.rules[0].declarations[0].value,
            Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            })
        );
        assert_eq!(
            stylesheet.rules[0].declarations[1].value,
            Value::Length(24.0, Unit::Px)
        );
        // Second rule: a lone `#app` is a single-branch list.
        assert_eq!(stylesheet.rules[1].selectors.list().len(), 1);
    }

    #[test]
    fn parses_descendant_selector_chain() {
        // Successful parse + a single comma-branch is the only contract
        // we still verify at this level — descendant matching itself is
        // covered in `style::tests::descendant_selector_matches_nested_target`.
        let stylesheet = parse(".outer .inner { color: red; }").unwrap();
        assert_eq!(stylesheet.rules[0].selectors.list().len(), 1);
    }

    #[test]
    fn parses_hover_pseudo_class_attached_to_simple_selector() {
        // The selectors crate encodes specificity in three bit-packed fields
        // (ids in the high byte, classes/pseudos in the middle, elements in
        // the low). `.btn:hover` is one class + one pseudo-class — both
        // count as "class-like" — so its specificity is two class units.
        let stylesheet = parse(".btn:hover { color: red; }").unwrap();
        assert_eq!(stylesheet.rules.len(), 1);
        let class_unit = parse(".x { }").unwrap().rules[0].selectors.specificity();
        assert_eq!(stylesheet.rules[0].selectors.specificity(), 2 * class_unit);
    }

    #[test]
    fn parses_focus_and_active_pseudo_classes() {
        let stylesheet = parse(
            r#"
                a:focus { color: red; }
                .btn:active { color: blue; }
            "#,
        )
        .unwrap();

        assert_eq!(stylesheet.rules.len(), 2);
        let element_unit = parse("a { }").unwrap().rules[0].selectors.specificity();
        let class_unit = parse(".x { }").unwrap().rules[0].selectors.specificity();
        // a:focus = 1 element + 1 pseudo-class.
        assert_eq!(
            stylesheet.rules[0].selectors.specificity(),
            element_unit + class_unit
        );
        // .btn:active = 1 class + 1 pseudo-class.
        assert_eq!(
            stylesheet.rules[1].selectors.specificity(),
            2 * class_unit
        );
    }

    #[test]
    fn parses_link_and_visited_pseudo_classes() {
        let stylesheet = parse(
            r#"
                a:link { color: red; }
                a:visited { color: blue; }
            "#,
        )
        .unwrap();

        assert_eq!(stylesheet.rules.len(), 2);
        // Same specificity (tag + pseudo); cascade tie-break is by source order
        // — that's what the style-layer `link_pseudo_*` tests exercise.
        let combined = stylesheet.rules[0].selectors.specificity();
        assert_eq!(stylesheet.rules[1].selectors.specificity(), combined);
        // Sanity: both selectors carry > 0 specificity.
        assert!(combined > 0);
    }

    #[test]
    fn unknown_pseudo_class_drops_the_whole_rule() {
        // Previously our hand-rolled parser silently turned `:totally-fake`
        // into "no pseudo" so the rule still applied to bare `.btn`. The
        // selectors crate is stricter — an unsupported pseudo-class is a
        // parse error, which our top-level rule iteration tolerates by
        // dropping the rule. Net result: `.btn:totally-fake` produces zero
        // matched rules, which is the spec-correct behaviour.
        let stylesheet = parse(".btn:totally-fake { color: red; }").unwrap();
        assert_eq!(stylesheet.rules.len(), 0);
    }

    #[test]
    fn parses_child_combinator_with_optional_surrounding_whitespace() {
        // Both forms must parse; matching semantics are covered in
        // `style::tests::child_selector_*`.
        assert!(parse(".outer > .inner { color: red; }").is_ok());
        assert!(parse(".outer>.inner { color: red; }").is_ok());
    }

    #[test]
    fn parses_mixed_descendant_and_child_combinators() {
        let stylesheet = parse("nav ul > li { display: block; }").unwrap();
        assert_eq!(stylesheet.rules.len(), 1);
        assert_eq!(stylesheet.rules[0].selectors.list().len(), 1);
    }

    #[test]
    fn descendant_chain_supports_three_levels() {
        let stylesheet = parse("nav ul li { display: block; }").unwrap();
        assert_eq!(stylesheet.rules.len(), 1);
        // Three tag selectors → 3 element units of specificity.
        let element_unit = parse("a { }").unwrap().rules[0].selectors.specificity();
        assert_eq!(stylesheet.rules[0].selectors.specificity(), 3 * element_unit);
    }

    #[test]
    fn parses_keyword_and_shorthand_hex_values() {
        let stylesheet = parse(
            r#"
                p {
                    display: block;
                    color: #0f0;
                }
            "#,
        )
        .unwrap();

        let declarations = &stylesheet.rules[0].declarations;
        assert_eq!(declarations[0].value, Value::Keyword("block".into()));
        assert_eq!(
            declarations[1].value,
            Value::Color(Color {
                r: 0,
                g: 255,
                b: 0,
                a: 255,
            })
        );
    }

    #[test]
    fn border_radius_single_value_expands_to_uniform_corners() {
        let stylesheet = parse("div { border-radius: 8px; }").unwrap();
        let names: Vec<&str> = stylesheet.rules[0]
            .declarations
            .iter()
            .map(|decl| decl.name.as_str())
            .collect();

        assert_eq!(
            names,
            vec![
                "border-top-left-radius",
                "border-top-right-radius",
                "border-bottom-right-radius",
                "border-bottom-left-radius",
            ]
        );
        for decl in &stylesheet.rules[0].declarations {
            assert_eq!(decl.value, Value::Length(8.0, Unit::Px));
        }
    }

    #[test]
    fn border_radius_two_values_pair_diagonal_corners() {
        let stylesheet = parse("div { border-radius: 8px 12px; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;

        assert_eq!(decls[0].name, "border-top-left-radius");
        assert_eq!(decls[0].value, Value::Length(8.0, Unit::Px));
        assert_eq!(decls[1].name, "border-top-right-radius");
        assert_eq!(decls[1].value, Value::Length(12.0, Unit::Px));
        assert_eq!(decls[2].name, "border-bottom-right-radius");
        assert_eq!(decls[2].value, Value::Length(8.0, Unit::Px));
        assert_eq!(decls[3].name, "border-bottom-left-radius");
        assert_eq!(decls[3].value, Value::Length(12.0, Unit::Px));
    }

    #[test]
    fn border_radius_four_values_assign_each_corner() {
        let stylesheet = parse("div { border-radius: 1px 2px 3px 4px; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;

        assert_eq!(decls[0].value, Value::Length(1.0, Unit::Px));
        assert_eq!(decls[1].value, Value::Length(2.0, Unit::Px));
        assert_eq!(decls[2].value, Value::Length(3.0, Unit::Px));
        assert_eq!(decls[3].value, Value::Length(4.0, Unit::Px));
    }

    #[test]
    fn border_radius_three_values_share_minor_diagonal() {
        let stylesheet = parse("div { border-radius: 1px 2px 3px; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;

        assert_eq!(decls[0].value, Value::Length(1.0, Unit::Px));
        assert_eq!(decls[1].value, Value::Length(2.0, Unit::Px));
        assert_eq!(decls[2].value, Value::Length(3.0, Unit::Px));
        assert_eq!(decls[3].value, Value::Length(2.0, Unit::Px));
    }

    #[test]
    fn explicit_corner_property_overrides_shorthand_when_listed_after() {
        let stylesheet =
            parse("div { border-radius: 8px; border-top-left-radius: 12px; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;

        assert_eq!(decls.len(), 5);
        assert_eq!(decls[4].name, "border-top-left-radius");
        assert_eq!(decls[4].value, Value::Length(12.0, Unit::Px));
    }

    #[test]
    fn parses_named_colors_into_color_values() {
        let stylesheet = parse(
            r#"
                .a { color: red; background-color: transparent; }
                .b { color: SkyBlue; border-color: cyan; }
            "#,
        )
        .unwrap();

        assert_eq!(
            stylesheet.rules[0].declarations[0].value,
            Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            })
        );
        assert_eq!(
            stylesheet.rules[0].declarations[1].value,
            Value::Color(Color {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            })
        );
        assert_eq!(
            stylesheet.rules[1].declarations[0].value,
            Value::Keyword("SkyBlue".into())
        );
        assert_eq!(
            stylesheet.rules[1].declarations[1].value,
            Value::Color(Color {
                r: 0,
                g: 255,
                b: 255,
                a: 255,
            })
        );
    }

    #[test]
    fn parses_rgb_function_into_opaque_color() {
        let stylesheet = parse("p { color: rgb(34, 68, 102); }").unwrap();
        assert_eq!(
            stylesheet.rules[0].declarations[0].value,
            Value::Color(Color {
                r: 34,
                g: 68,
                b: 102,
                a: 255,
            })
        );
    }

    #[test]
    fn parses_rgba_function_with_fractional_alpha() {
        let stylesheet = parse("p { background-color: rgba(255, 0, 0, 0.5); }").unwrap();
        assert_eq!(
            stylesheet.rules[0].declarations[0].value,
            Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 128,
            })
        );
    }

    #[test]
    fn rgb_function_clamps_out_of_range_components() {
        let stylesheet = parse("p { color: rgb(300, 12, 0); }").unwrap();
        assert_eq!(
            stylesheet.rules[0].declarations[0].value,
            Value::Color(Color {
                r: 255,
                g: 12,
                b: 0,
                a: 255,
            })
        );
    }

    #[test]
    fn parses_percent_em_and_rem_length_units() {
        let stylesheet = parse(
            r#"
                .a {
                    width: 50%;
                    padding: 1.5em;
                    font-size: 0.875rem;
                }
            "#,
        )
        .unwrap();

        let decls = &stylesheet.rules[0].declarations;
        assert_eq!(decls[0].name, "width");
        assert_eq!(decls[0].value, Value::Length(50.0, Unit::Percent));
        assert_eq!(decls[1].name, "padding");
        assert_eq!(decls[1].value, Value::Length(1.5, Unit::Em));
        assert_eq!(decls[2].name, "font-size");
        assert_eq!(decls[2].value, Value::Length(0.875, Unit::Rem));
    }

    #[test]
    fn skips_invalid_declarations() {
        let stylesheet = parse("div { color red; font-size: 16px; }").unwrap();

        assert_eq!(stylesheet.rules.len(), 1);
        assert_eq!(stylesheet.rules[0].declarations.len(), 1);
        assert_eq!(stylesheet.rules[0].declarations[0].name, "font-size");
    }

    #[test]
    fn parses_unitless_integer_as_number() {
        let stylesheet = parse(".a { z-index: 5; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;

        assert_eq!(decls[0].name, "z-index");
        assert_eq!(decls[0].value, Value::Number(5.0));
    }

    #[test]
    fn parses_negative_number_for_z_index() {
        let stylesheet = parse(".a { z-index: -2; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;

        assert_eq!(decls[0].value, Value::Number(-2.0));
    }

    #[test]
    fn parses_negative_length_with_unit() {
        let stylesheet = parse(".a { margin-left: -10px; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;

        assert_eq!(decls[0].value, Value::Length(-10.0, Unit::Px));
    }

    #[test]
    fn parses_unitless_decimal_as_number() {
        let stylesheet = parse(".a { line-height: 1.5; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;

        assert_eq!(decls[0].value, Value::Number(1.5));
    }

    #[test]
    fn parses_linear_gradient_default_direction_and_auto_stops() {
        let stylesheet = parse(".a { background-image: linear-gradient(red, blue); }").unwrap();
        let decl = &stylesheet.rules[0].declarations[0];

        let gradient = match &decl.value {
            Value::Gradient(gradient) => gradient,
            other => panic!("expected Gradient, got {other:?}"),
        };
        assert_eq!(
            gradient.kind,
            GradientKind::Linear(GradientDirection::ToBottom)
        );
        assert_eq!(gradient.stops.len(), 2);
        assert_eq!(gradient.stops[0].position, None);
        assert_eq!(gradient.stops[1].position, None);
        assert_eq!(gradient.stops[0].color.r, 255);
        assert_eq!(gradient.stops[1].color.b, 255);
    }

    #[test]
    fn parses_linear_gradient_with_explicit_direction() {
        let stylesheet =
            parse(".a { background-image: linear-gradient(to right, red, blue); }").unwrap();
        let gradient = match &stylesheet.rules[0].declarations[0].value {
            Value::Gradient(gradient) => gradient,
            other => panic!("expected Gradient, got {other:?}"),
        };
        assert_eq!(
            gradient.kind,
            GradientKind::Linear(GradientDirection::ToRight)
        );
    }

    #[test]
    fn parses_box_shadow_full_form() {
        let stylesheet = parse(".a { box-shadow: 5px 10px 15px 2px black; }").unwrap();
        let value = &stylesheet.rules[0].declarations[0].value;
        let shadow = match value {
            Value::BoxShadow(shadow) => *shadow,
            other => panic!("expected BoxShadow, got {other:?}"),
        };
        assert_eq!(
            shadow,
            BoxShadow {
                offset_x: 5.0,
                offset_y: 10.0,
                blur_radius: 15.0,
                spread_radius: 2.0,
                color: Color {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 255,
                },
            }
        );
    }

    #[test]
    fn parses_box_shadow_minimal_form_uses_defaults() {
        let stylesheet = parse(".a { box-shadow: 5px 10px; }").unwrap();
        let shadow = match &stylesheet.rules[0].declarations[0].value {
            Value::BoxShadow(shadow) => *shadow,
            other => panic!("expected BoxShadow, got {other:?}"),
        };
        assert_eq!(shadow.offset_x, 5.0);
        assert_eq!(shadow.offset_y, 10.0);
        assert_eq!(shadow.blur_radius, 0.0);
        assert_eq!(shadow.spread_radius, 0.0);
        assert_eq!(
            shadow.color,
            Color {
                r: 0,
                g: 0,
                b: 0,
                a: 255
            }
        );
    }

    #[test]
    fn parses_box_shadow_with_negative_offsets() {
        let stylesheet = parse(".a { box-shadow: -3px -4px 8px red; }").unwrap();
        let shadow = match &stylesheet.rules[0].declarations[0].value {
            Value::BoxShadow(shadow) => *shadow,
            other => panic!("expected BoxShadow, got {other:?}"),
        };
        assert_eq!(shadow.offset_x, -3.0);
        assert_eq!(shadow.offset_y, -4.0);
        assert_eq!(shadow.blur_radius, 8.0);
        assert_eq!(shadow.color.r, 255);
    }

    #[test]
    fn parses_text_shadow_full_form() {
        let stylesheet = parse(".a { text-shadow: 2px 3px 4px red; }").unwrap();
        let shadow = match &stylesheet.rules[0].declarations[0].value {
            Value::TextShadow(shadow) => *shadow,
            other => panic!("expected TextShadow, got {other:?}"),
        };
        assert_eq!(
            shadow,
            TextShadow {
                offset_x: 2.0,
                offset_y: 3.0,
                blur_radius: 4.0,
                color: Color {
                    r: 255,
                    g: 0,
                    b: 0,
                    a: 255,
                },
            }
        );
    }

    #[test]
    fn parses_text_shadow_minimal_form_uses_defaults() {
        let stylesheet = parse(".a { text-shadow: 2px 3px; }").unwrap();
        let shadow = match &stylesheet.rules[0].declarations[0].value {
            Value::TextShadow(shadow) => *shadow,
            other => panic!("expected TextShadow, got {other:?}"),
        };
        assert_eq!(shadow.offset_x, 2.0);
        assert_eq!(shadow.offset_y, 3.0);
        assert_eq!(shadow.blur_radius, 0.0);
        assert_eq!(
            shadow.color,
            Color {
                r: 0,
                g: 0,
                b: 0,
                a: 255
            }
        );
    }

    #[test]
    fn parses_radial_gradient_as_radial_kind() {
        let stylesheet = parse(".a { background-image: radial-gradient(red, blue); }").unwrap();
        let gradient = match &stylesheet.rules[0].declarations[0].value {
            Value::Gradient(gradient) => gradient,
            other => panic!("expected Gradient, got {other:?}"),
        };
        assert_eq!(gradient.kind, GradientKind::Radial);
        assert_eq!(gradient.stops.len(), 2);
    }

    #[test]
    fn parses_double_quoted_url_as_image_url() {
        let stylesheet = parse(r#".a { background-image: url("img/logo.png"); }"#).unwrap();
        let value = &stylesheet.rules[0].declarations[0].value;
        assert_eq!(value, &Value::ImageUrl("img/logo.png".to_string()));
    }

    #[test]
    fn parses_single_quoted_url_as_image_url() {
        let stylesheet = parse(".a { background-image: url('logo.png'); }").unwrap();
        let value = &stylesheet.rules[0].declarations[0].value;
        assert_eq!(value, &Value::ImageUrl("logo.png".to_string()));
    }

    #[test]
    fn parses_unquoted_url_trims_outer_whitespace() {
        let stylesheet = parse(".a { background-image: url(  /static/bg.png  ); }").unwrap();
        let value = &stylesheet.rules[0].declarations[0].value;
        assert_eq!(value, &Value::ImageUrl("/static/bg.png".to_string()));
    }

    #[test]
    fn parses_transform_translate_two_args() {
        let stylesheet = parse(".a { transform: translate(10px, 20px); }").unwrap();
        let value = &stylesheet.rules[0].declarations[0].value;
        let ops = match value {
            Value::TransformList(ops) => ops,
            other => panic!("expected TransformList, got {other:?}"),
        };
        assert_eq!(
            ops.as_slice(),
            &[TransformOp::Translate { x: 10.0, y: 20.0 }]
        );
    }

    #[test]
    fn parses_transform_translate_one_arg_defaults_y_to_zero() {
        let stylesheet = parse(".a { transform: translate(10px); }").unwrap();
        let ops = match &stylesheet.rules[0].declarations[0].value {
            Value::TransformList(ops) => ops.clone(),
            other => panic!("expected TransformList, got {other:?}"),
        };
        assert_eq!(
            ops.as_slice(),
            &[TransformOp::Translate { x: 10.0, y: 0.0 }]
        );
    }

    #[test]
    fn parses_transform_translate_axis_helpers() {
        let stylesheet = parse(".a { transform: translateX(5px) translateY(-7px); }").unwrap();
        let ops = match &stylesheet.rules[0].declarations[0].value {
            Value::TransformList(ops) => ops.clone(),
            other => panic!("expected TransformList, got {other:?}"),
        };
        assert_eq!(
            ops.as_slice(),
            &[
                TransformOp::Translate { x: 5.0, y: 0.0 },
                TransformOp::Translate { x: 0.0, y: -7.0 },
            ]
        );
    }

    #[test]
    fn parses_transform_scale_uniform_one_arg() {
        let stylesheet = parse(".a { transform: scale(2); }").unwrap();
        let ops = match &stylesheet.rules[0].declarations[0].value {
            Value::TransformList(ops) => ops.clone(),
            other => panic!("expected TransformList, got {other:?}"),
        };
        assert_eq!(ops.as_slice(), &[TransformOp::Scale { x: 2.0, y: 2.0 }]);
    }

    #[test]
    fn parses_transform_scale_two_args_and_axis_helpers() {
        let stylesheet =
            parse(".a { transform: scale(1.5, 0.5) scaleX(3) scaleY(0.25); }").unwrap();
        let ops = match &stylesheet.rules[0].declarations[0].value {
            Value::TransformList(ops) => ops.clone(),
            other => panic!("expected TransformList, got {other:?}"),
        };
        assert_eq!(
            ops.as_slice(),
            &[
                TransformOp::Scale { x: 1.5, y: 0.5 },
                TransformOp::Scale { x: 3.0, y: 1.0 },
                TransformOp::Scale { x: 1.0, y: 0.25 },
            ]
        );
    }

    #[test]
    fn parses_transform_rotate_in_degrees_and_radians() {
        let stylesheet = parse(".a { transform: rotate(45deg) rotate(2rad); }").unwrap();
        let ops = match &stylesheet.rules[0].declarations[0].value {
            Value::TransformList(ops) => ops.clone(),
            other => panic!("expected TransformList, got {other:?}"),
        };
        let expected = [std::f32::consts::FRAC_PI_4, 2.0];
        let actual: Vec<f32> = ops
            .iter()
            .map(|op| match op {
                TransformOp::Rotate(theta) => *theta,
                other => panic!("expected Rotate, got {other:?}"),
            })
            .collect();
        for (got, want) in actual.iter().zip(expected.iter()) {
            assert!((got - want).abs() < 1e-4, "got {got}, want {want}");
        }
    }

    #[test]
    fn parses_transform_rotate_negative_and_turn_unit() {
        let stylesheet = parse(".a { transform: rotate(-0.25turn); }").unwrap();
        let ops = match &stylesheet.rules[0].declarations[0].value {
            Value::TransformList(ops) => ops.clone(),
            other => panic!("expected TransformList, got {other:?}"),
        };
        match ops.as_slice() {
            [TransformOp::Rotate(theta)] => {
                assert!((theta + std::f32::consts::FRAC_PI_2).abs() < 1e-4);
            }
            other => panic!("expected single Rotate, got {other:?}"),
        }
    }

    #[test]
    fn parses_linear_gradient_with_explicit_stop_positions() {
        let stylesheet =
            parse(".a { background-image: linear-gradient(red 0%, yellow 50%, blue 100%); }")
                .unwrap();
        let gradient = match &stylesheet.rules[0].declarations[0].value {
            Value::Gradient(gradient) => gradient,
            other => panic!("expected Gradient, got {other:?}"),
        };
        let positions: Vec<_> = gradient.stops.iter().map(|s| s.position).collect();
        assert_eq!(positions, vec![Some(0.0), Some(0.5), Some(1.0)]);
    }

    #[test]
    fn grid_column_parses_index_slash_index() {
        let stylesheet = parse(".a { grid-column: 1 / 3; }").unwrap();
        let value = &stylesheet.rules[0].declarations[0].value;
        match value {
            Value::GridPlacement(p) => {
                assert_eq!(p.start, GridLine::Index(1));
                assert_eq!(p.end, GridLine::Index(3));
            }
            other => panic!("expected GridPlacement, got {other:?}"),
        }
    }

    #[test]
    fn grid_column_parses_span_form() {
        let stylesheet = parse(".a { grid-column: 2 / span 4; }").unwrap();
        match &stylesheet.rules[0].declarations[0].value {
            Value::GridPlacement(p) => {
                assert_eq!(p.start, GridLine::Index(2));
                assert_eq!(p.end, GridLine::Span(4));
            }
            other => panic!("expected GridPlacement, got {other:?}"),
        }
    }

    #[test]
    fn grid_row_single_index_defaults_end_to_auto() {
        let stylesheet = parse(".a { grid-row: 5; }").unwrap();
        match &stylesheet.rules[0].declarations[0].value {
            Value::GridPlacement(p) => {
                assert_eq!(p.start, GridLine::Index(5));
                assert_eq!(p.end, GridLine::Auto);
            }
            other => panic!("expected GridPlacement, got {other:?}"),
        }
    }

    #[test]
    fn grid_template_areas_parses_quoted_rows_into_cell_map() {
        let stylesheet =
            parse(r#".g { grid-template-areas: "h h h" "s m m" "f f f"; }"#).unwrap();
        match &stylesheet.rules[0].declarations[0].value {
            Value::TemplateAreas(rows) => {
                assert_eq!(rows.len(), 3);
                assert_eq!(rows[0], vec![Some("h".into()), Some("h".into()), Some("h".into())]);
                assert_eq!(rows[1], vec![Some("s".into()), Some("m".into()), Some("m".into())]);
                assert_eq!(rows[2], vec![Some("f".into()), Some("f".into()), Some("f".into())]);
            }
            other => panic!("expected TemplateAreas, got {other:?}"),
        }
    }

    #[test]
    fn grid_template_areas_recognizes_dot_as_empty_cell() {
        let stylesheet = parse(r#".g { grid-template-areas: "a . b"; }"#).unwrap();
        match &stylesheet.rules[0].declarations[0].value {
            Value::TemplateAreas(rows) => {
                assert_eq!(rows[0], vec![Some("a".into()), None, Some("b".into())]);
            }
            other => panic!("expected TemplateAreas, got {other:?}"),
        }
    }

    #[test]
    fn flex_shorthand_one_number_emits_grow_with_default_shrink() {
        let stylesheet = parse(".a { flex: 2; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;
        assert_eq!(decls.len(), 2);
        assert_eq!(decls[0].name, "flex-grow");
        assert_eq!(decls[0].value, Value::Number(2.0));
        assert_eq!(decls[1].name, "flex-shrink");
        assert_eq!(decls[1].value, Value::Number(1.0));
    }

    #[test]
    fn grid_template_columns_parses_lengths_and_fractions() {
        let stylesheet = parse(".g { grid-template-columns: 100px 1fr 2fr 50px; }").unwrap();
        let value = &stylesheet.rules[0].declarations[0].value;
        let tracks = match value {
            Value::TrackList(tracks) => tracks,
            other => panic!("expected TrackList, got {other:?}"),
        };
        assert_eq!(tracks.len(), 4);
        assert_eq!(tracks[0], TrackSize::Length(100.0, Unit::Px));
        assert_eq!(tracks[1], TrackSize::Fraction(1.0));
        assert_eq!(tracks[2], TrackSize::Fraction(2.0));
        assert_eq!(tracks[3], TrackSize::Length(50.0, Unit::Px));
    }

    #[test]
    fn grid_template_columns_rejects_unitless_number() {
        let stylesheet = parse(".g { grid-template-columns: 5; }");
        if let Ok(stylesheet) = stylesheet {
            for rule in stylesheet.rules {
                for decl in rule.declarations {
                    if decl.name == "grid-template-columns"
                        && let Value::TrackList(tracks) = decl.value
                    {
                        assert!(tracks.is_empty(), "unitless number should not parse");
                    }
                }
            }
        }
    }

    #[test]
    fn flex_shorthand_three_values_emits_all_longhands() {
        let stylesheet = parse(".a { flex: 1 0 80px; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;
        assert_eq!(decls.len(), 3);
        assert_eq!(
            (&decls[0].name, &decls[0].value),
            (&"flex-grow".to_string(), &Value::Number(1.0))
        );
        assert_eq!(
            (&decls[1].name, &decls[1].value),
            (&"flex-shrink".to_string(), &Value::Number(0.0))
        );
        assert_eq!(
            (&decls[2].name, &decls[2].value),
            (&"flex-basis".to_string(), &Value::Length(80.0, Unit::Px))
        );
    }
}
