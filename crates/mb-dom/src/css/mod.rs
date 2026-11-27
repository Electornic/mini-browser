// CSS support is now driven by Servo's `selectors` crate for selector
// parsing + matching, and by `cssparser` for the rest of the rule body.
// The selector AST is no longer ours — `Selector` is a thin newtype around
// `selectors::SelectorList<MiniBrowserSelectorImpl>` so callers can hold
// owned, parsed selector lists without dragging the generic parameter
// through every file.
mod color;
mod error;
mod gradient;
mod grid;
pub mod media;
mod parse;
pub mod properties;
mod report;
mod selector;
mod shadow;
mod shorthand;
mod transform;

use grid::{
    parse_grid_placement, parse_grid_template_areas, parse_grid_template_shorthand,
    parse_grid_track_list,
};
use parse::{is_parse_only, parse_clip, parse_font_family, parse_raw, parse_value};
pub use report::{
    AtRuleSeen, DropReason, DroppedDeclaration, DroppedRule, MediaVerdict, Origin, ParseReport,
    media_verdict, parse_with_report,
};
use selector::SelectorParserImpl;
pub use selector::{CssString, MiniBrowserSelectorImpl, NonTSPseudoClass, PseudoElement, Selector};
use shadow::{parse_box_shadow_value, parse_text_shadow_value};
use shorthand::{
    parse_background_position, parse_background_shorthand, parse_background_size,
    parse_border_radius_shorthand, parse_border_shorthand, parse_border_side_width,
    parse_border_sides, parse_box_edge_shorthand, parse_flex_shorthand, parse_inset_shorthand,
};
use transform::parse_transform_value;

use std::rc::Rc;

use media::{MAX_MEDIA_NESTING, MediaBlock, MediaQueryList};

use cssparser::{
    AtRuleParser as CssAtRuleParser, CowRcStr, ParseError as CssParseError, Parser as CssParser,
    ParserState, QualifiedRuleParser as CssQualifiedRuleParser, StyleSheetParser,
};
use selectors::parser::{ParseRelative, SelectorList as SelectorsSelectorList};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
    /// Every `@media` block, in source order (an enclosing block before
    /// the ones nested in it); `MediaBlock::index` is the position here.
    pub media: Vec<Rc<MediaBlock>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    /// Comma-separated selector list (e.g. `h1, .title`). Wrapped in a
    /// newtype so the `selectors`-crate generic doesn't leak into our
    /// public surface and so we can derive `PartialEq` for snapshot tests.
    pub selectors: Selector,
    pub declarations: Vec<Declaration>,
    /// The innermost `@media` block the rule sits in, shared by every rule
    /// of that block (`None` = applies everywhere). The cascade skips the
    /// rule when the block's chain doesn't match.
    pub media: Option<Rc<MediaBlock>>,
    /// Some declaration is `!important` — the cascade's second (important)
    /// pass skips rules without one.
    pub has_important: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    pub name: String,
    pub value: Value,
    /// `!important` (compat round 3 step 1a). A shorthand's longhands all
    /// carry the shorthand's flag.
    pub important: bool,
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
    // `font-family` list, in order, names unquoted (step 1b). Only the
    // monospace check reads it today; real font selection is out of scope.
    FontFamilies(Vec<String>),
    // `background-size` (step 1b) — painted by `background_image_command`.
    BackgroundSize(BackgroundSize),
    // `clip: rect(…)` / `clip-path: inset(…)` (step 1b) — kept for the
    // overflow-clipping step (compat round 3 step 3); nothing reads it yet.
    ClipBox(ClipBox),
    // A value kept as source text until the cascade (step 1c): every
    // custom property (`--*`, the spec's token sequence), and any value
    // containing `var(` — substituted, then re-parsed after the cascade
    // (`style::variables`). A shorthand's value is stored on each of its
    // longhands with `shorthand` set, so the cascade orders longhands as
    // usual and the re-parse runs the shorthand parser. Never reaches the
    // styled tree.
    Unparsed {
        shorthand: Option<String>,
        raw: String,
    },
    // Removal candidate: the parser no longer produces it (step 1c routes
    // every `var(` through `Unparsed`).
    // `var(--name)` / `var(--name, fallback)` reference. Survives parse time
    // unresolved; the cascade in `style.rs` walks every value at style time
    // and substitutes the looked-up `--*` declaration in scope (custom
    // properties inherit per spec). Unresolved references — name not in
    // scope and no fallback — fall through as `Keyword("initial")`.
    Var {
        name: String,
        fallback: Option<Box<Value>>,
    },
}

/// `background-size`: a keyword, or a width and height (`auto` keeps the
/// image's ratio; both `auto` = natural size).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BackgroundSize {
    Cover,
    Contain,
    Size(SizeAxis, SizeAxis),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SizeAxis {
    Auto,
    Length(f32, Unit),
}

/// `clip: rect(top, right, bottom, left)` (edges from the box's top-left)
/// or `clip-path: inset(top right bottom left)` (insets from each side).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipBox {
    pub function: ClipFunction,
    /// top, right, bottom, left.
    pub edges: [ClipEdge; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipFunction {
    Rect,
    Inset,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClipEdge {
    Auto,
    Length(f32, Unit),
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
    /// `min-content` / `max-content` tracks. The legacy grid measures one
    /// natural width per item, so both size to it there; taffy tells them
    /// apart.
    MinContent,
    MaxContent,
    /// `minmax(min, max)`. `min` is never `Fraction` (the parser rejects a
    /// flexible minimum, as browsers do).
    MinMax(TrackBreadth, TrackBreadth),
}

/// One side of `minmax()`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrackBreadth {
    Length(f32, Unit),
    Fraction(f32),
    Auto,
    MinContent,
    MaxContent,
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
    // `ch` is the advance width of the glyph "0" in the element's font; resolved at
    // style time alongside em/rem. We approximate it as `0.5 * font-size`, which is
    // close enough for the typical proportional fonts pages use to set reading
    // widths (`max-width: 65ch` ≈ 8 words / line at 16px). A real implementation
    // would query cosmic-text for the "0" glyph advance.
    Ch,
    // Absolute typographic unit: 1pt = 1/72in, with CSS pinning 1in = 96px → 1pt = 4/3 px.
    // Resolved to Px during the cascade alongside em/rem so downstream layout only
    // sees Px / Percent. Common on legacy pages that set `font-size: 10pt` on body.
    Pt,
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
    let mut parser = CssParser::new(source);
    let mut media = Vec::new();
    let mut rules = Vec::new();
    {
        let mut handler = StylesheetHandler {
            media: None,
            depth: 0,
            blocks: &mut media,
        };
        let iter = StyleSheetParser::new(&mut parser, &mut handler);
        // Tolerant recovery: `StyleSheetParser` itself walks past a broken rule's
        // block before yielding the next item, so we just keep the successes and
        // discard the errors — same semantic as the previous `skip_to_end_of_block`.
        // Each yielded item is a Vec<Rule>: top-level qualified rules contribute
        // a single entry, `@media` blocks contribute N (Phase 6.F).
        collect_rules(iter, &mut rules);
    }
    Ok(Stylesheet { rules, media })
}

/// Parse the contents of a `style="..."` attribute as a flat declaration
/// list (no surrounding braces). The returned declarations carry the same
/// shape as a stylesheet rule's body — including longhand expansion for
/// shorthand properties — so the cascade applies them through the same
/// `apply_declarations` path. Errors on individual declarations are
/// tolerated; the offending entry is dropped and the rest continue,
/// matching the stylesheet parser's recovery behaviour.
pub fn parse_inline_style(source: &str) -> Vec<Declaration> {
    let mut parser = CssParser::new(source);
    parse_declaration_block(&mut parser).unwrap_or_default()
}

/// Parse a single complex selector (e.g. `div.card > a`). Used by JS-facing
/// `document.querySelector` to reuse the same selector grammar as the
/// stylesheet parser without going through a full stylesheet rule.
/// Trailing input after the selector is rejected so callers can't smuggle
/// declarations through the same entry point.
pub fn parse_selector(input: &str) -> Result<Selector, ParseError> {
    let mut parser = CssParser::new(input);
    let parsed = parser
        .parse_entirely(|input| {
            SelectorsSelectorList::parse(&SelectorParserImpl, input, ParseRelative::No)
        })
        .map_err(convert_selector_error)?;
    Ok(Selector(parsed))
}

/// Convert a `selectors`-crate parse error into our `ParseError` shape.
/// We lose the structured `SelectorParseErrorKind` variant data because
/// it carries `Token<'_>` borrows, but the message string is enough for
/// the JS bridge's `SyntaxError` payload and the legacy stylesheet
/// recovery path drops parse failures wholesale anyway.
fn convert_selector_error(
    err: CssParseError<selectors::parser::SelectorParseErrorKind>,
) -> ParseError {
    let position = 0; // cssparser 0.38 errors no longer carry a location
    let message = format!("{:?}", err.kind);
    ParseError::new(position, message)
}

// =============================================================================
// cssparser glue: rule iteration via StyleSheetParser
// =============================================================================

struct StylesheetHandler<'a> {
    /// The `@media` block being parsed (`None` at the top level).
    media: Option<Rc<MediaBlock>>,
    /// How many `@media` blocks enclose this one.
    depth: usize,
    /// The sheet's block list, shared by nested handlers.
    blocks: &'a mut Vec<Rc<MediaBlock>>,
}

/// Keep every parsed rule; note the selector of each one dropped (an
/// at-rule that failed is already in the report under its name).
fn collect_rules<'i, I>(iter: I, rules: &mut Vec<Rule>)
where
    I: Iterator<
        Item = Result<Vec<Rule>, (CssParseError<CssError>, &'i str, cssparser::SourceLocation)>,
    >,
{
    for item in iter {
        match item {
            Ok(batch) => rules.extend(batch),
            Err((_, source, _)) if !source.trim_start().starts_with('@') => {
                report::record(|r| r.dropped_rules.push(report::dropped_rule(source)))
            }
            Err(_) => {}
        }
    }
}

/// Bundle a parsed selector list with the source location where it
/// started, so `parse_block` can build a `Rule` with the original prelude.
struct RulePrelude(Selector);

/// `cssparser` requires a custom error type; the toy parser converts everything
/// back into the legacy `ParseError` shape on the way out.
type CssError = ParseError;

impl<'i> CssQualifiedRuleParser<'i> for StylesheetHandler<'_> {
    type Prelude = RulePrelude;
    // Vec<Rule> instead of a single `Rule` so the parse() iterator can fold
    // both qualified rules and at-rule expansions through the same channel.
    // Top-level `.foo {…}` always returns a single-element vec; `@media`
    // expands to N entries (Phase 6.F).
    type QualifiedRule = Vec<Rule>;
    type Error = CssError;

    fn parse_prelude<'t>(
        &mut self,
        input: &mut CssParser<'i>,
    ) -> Result<Self::Prelude, CssParseError<Self::Error>> {
        // Delegate the whole comma-separated selector grammar to the
        // selectors crate — it handles compound selectors, attribute
        // selectors, the descendant/child/sibling combinators,
        // pseudo-classes (via our `Parser` impl), `:is()`/`:where()`/
        // `:not()`, etc. We don't enable `parse_is_and_where` / `parse_has`
        // / `parse_part` / `parse_slotted` because we don't model the
        // matching machinery for them yet.
        match SelectorsSelectorList::parse(&SelectorParserImpl, input, ParseRelative::No) {
            Ok(list) => Ok(RulePrelude(Selector(list))),
            Err(err) => Err(CssParseError::custom(convert_selector_error(err))),
        }
    }

    fn parse_block<'t>(
        &mut self,
        prelude: Self::Prelude,
        _start: &ParserState,
        input: &mut CssParser<'i>,
    ) -> Result<Self::QualifiedRule, CssParseError<Self::Error>> {
        let declarations = parse_declaration_block(input).map_err(CssParseError::custom)?;
        Ok(vec![Rule {
            selectors: prelude.0,
            has_important: declarations.iter().any(|d| d.important),
            declarations,
            media: self.media.clone(),
        }])
    }
}

/// Discriminator the at-rule parser hands from `parse_prelude` to
/// `parse_block`. We model only `@media` today; everything else trips the
/// `Reject` arm so `parse_block` can return an empty Vec without invoking
/// any parsing.
enum AtRulePrelude {
    /// The parsed query list, and the block's index in the parse report
    /// (when one is active).
    Media(MediaQueryList, Option<usize>),
    Reject,
}

impl<'i> CssAtRuleParser<'i> for StylesheetHandler<'_> {
    type Prelude = AtRulePrelude;
    // Vec<Rule>: `@media` unfolds into N qualified rules (Phase 6.F),
    // every other at-rule yields an empty vec.
    type AtRule = Vec<Rule>;
    type Error = CssError;

    fn parse_prelude<'t>(
        &mut self,
        name: cssparser::CowRcStr<'i>,
        input: &mut CssParser<'i>,
    ) -> Result<Self::Prelude, CssParseError<Self::Error>> {
        // `@media`: parse the query list (evaluated per style pass, see
        // `media::evaluate_blocks`) and let `parse_block` recurse into the
        // body. Every other at-rule's prelude is consumed and dropped.
        let is_media = name.eq_ignore_ascii_case("media");
        let start = input.position();
        let queries = if is_media {
            MediaQueryList::parse_from(input)
        } else {
            while input.next().is_ok() {}
            MediaQueryList::default()
        };
        let seen = report::record_at_rule(&name, input.slice_from(start), is_media);
        if is_media {
            return Ok(AtRulePrelude::Media(queries, seen));
        }
        // Other at-rules (@charset, @keyframes, @font-face, …) keep the
        // pre-6.F behaviour: skip the prelude + body. parse_block returns
        // an empty Vec and the iter::flatten in `parse()` quietly drops it.
        Ok(AtRulePrelude::Reject)
    }

    fn parse_block<'t>(
        &mut self,
        prelude: Self::Prelude,
        _start: &ParserState,
        input: &mut CssParser<'i>,
    ) -> Result<Self::AtRule, CssParseError<Self::Error>> {
        match prelude {
            AtRulePrelude::Media(queries, seen) => {
                // Recursion is bounded: past the cap the block's rules are
                // dropped (cssparser skips the rest of the body).
                if self.depth >= MAX_MEDIA_NESTING {
                    return Ok(Vec::new());
                }
                // Recurse into the @media body with a handler that tags
                // every rule with this block; a nested `@media` chains onto
                // it through `parent` (nested conditions AND together).
                let block = Rc::new(MediaBlock {
                    queries,
                    parent: self.media.clone(),
                    index: self.blocks.len(),
                });
                self.blocks.push(Rc::clone(&block));
                report::enter_media(seen);
                let mut handler = StylesheetHandler {
                    media: Some(block),
                    depth: self.depth + 1,
                    blocks: &mut *self.blocks,
                };
                let iter = StyleSheetParser::new(input, &mut handler);
                let mut rules = Vec::new();
                collect_rules(iter, &mut rules);
                report::leave_media(seen, rules.len());
                Ok(rules)
            }
            AtRulePrelude::Reject => {
                if report::is_active() {
                    let start = input.position();
                    while input.next().is_ok() {}
                    let bytes = input.slice_from(start).len();
                    report::record(|r| {
                        if let Some(seen) = r.at_rules.last_mut() {
                            seen.skipped_bytes = bytes;
                        }
                    });
                }
                Ok(Vec::new())
            }
        }
    }

    fn rule_without_block(
        &mut self,
        _prelude: Self::Prelude,
        _start: &ParserState,
    ) -> Result<Self::AtRule, ()> {
        // Block-less forms (`@charset "utf-8";`) — drop quietly.
        Ok(Vec::new())
    }
}

// =============================================================================
// Declaration block parsing
// =============================================================================

/// Iterate the `{ ... }` body and collect declarations. Errors on individual
/// declarations are tolerated: the offending entry is dropped and the next one
/// continues. `RuleBodyParser` already scans past the next semicolon on error
/// so the tolerant behaviour matches the original parser.
fn parse_declaration_block<'i>(input: &mut CssParser<'i>) -> Result<Vec<Declaration>, ParseError> {
    let mut declarations = Vec::new();
    let mut handler = DeclHandler;
    let iter = cssparser::RuleBodyParser::new(input, &mut handler);
    for item in iter {
        match item {
            Ok(decls) => declarations.extend(decls),
            // Classify outside `record`: it may re-parse the declaration.
            Err((_, source, _)) if report::is_active() => {
                let dropped = report::dropped_declaration(source);
                report::record(|r| r.dropped_declarations.push(dropped));
            }
            Err(_) => {}
        }
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
        input: &mut CssParser<'i>,
        _decl_start: &ParserState,
    ) -> Result<Self::Declaration, CssParseError<Self::Error>> {
        // Dispatch by property name to mirror the old `parse_declaration`. The
        // value-shape helpers each consume their delimited slice end-to-end so
        // `parse_value` itself just routes by name.
        let name = name.to_string();
        // The value ends before a `!`; what follows must be exactly
        // `!important` (any case, whitespace/comments allowed) — anything
        // else (`!ie`, `!important foo`) leaves input behind and the
        // declaration drops, as in Chrome. Value parsers never see the flag.
        let mut decls = input.parse_until_before(cssparser::Delimiter::Bang, |input| {
            // Custom properties are token sequences, and a value with
            // `var()` can only be parsed once substituted (step 1c): keep
            // both as source text for the cascade.
            let custom = name.starts_with("--");
            let state = input.state();
            let has_var = scan_var_references(input).map_err(CssParseError::custom)?;
            input.reset(&state);
            if custom || has_var {
                return Ok(unparsed_declarations(&name, take_raw(input)));
            }
            parse_declaration_value(&name, input).map_err(CssParseError::custom)
        })?;
        if input.try_parse(cssparser::parse_important).is_ok() {
            for decl in &mut decls {
                decl.important = true;
            }
        }
        Ok(decls)
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
fn parse_declaration_value<'i>(
    name: &str,
    input: &mut CssParser<'i>,
) -> Result<Vec<Declaration>, ParseError> {
    // CSS-wide keywords are valid for every property; a shorthand passes
    // them to each of its longhands (the cascade resolves them, step 1b).
    if let Ok(keyword) = input.try_parse(parse_css_wide_keyword) {
        return Ok(longhands_of(name)
            .into_iter()
            .map(|longhand| Declaration {
                name: longhand,
                value: Value::Keyword(keyword.clone()),
                important: false,
            })
            .collect());
    }
    if name == "all" {
        return Err(ParseError::new(
            input.position().byte_index(),
            "all: only CSS-wide keywords",
        ));
    }
    let single = |value: Value| {
        Ok(vec![Declaration {
            name: name.to_string(),
            value,
            important: false,
        }])
    };
    match name {
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" => {
            return parse_border_shorthand(name, input);
        }
        "border-width" | "border-style" | "border-color" => return parse_border_sides(name, input),
        "border-top-width" | "border-right-width" | "border-bottom-width" | "border-left-width" => {
            let side = &name["border-".len()..name.len() - "-width".len()];
            return parse_border_side_width(side, input);
        }
        "inset" => return parse_inset_shorthand(input),
        "font-family" => return single(parse_font_family(input)?),
        "background-size" => return single(parse_background_size(input)?),
        "clip" | "clip-path" => return single(parse_clip(name, input)?),
        _ if is_parse_only(name) => return single(parse_raw(input)?),
        _ => {}
    }
    if name == "border-radius" {
        return parse_border_radius_shorthand(name, input);
    }
    if name == "padding" || name == "margin" {
        return parse_box_edge_shorthand(name, input);
    }
    if name == "box-shadow" {
        let value = parse_box_shadow_value(input)?;
        return Ok(vec![Declaration {
            name: name.to_string(),
            value,
            important: false,
        }]);
    }
    if name == "text-shadow" {
        let value = parse_text_shadow_value(input)?;
        return Ok(vec![Declaration {
            name: name.to_string(),
            value,
            important: false,
        }]);
    }
    if name == "transform" {
        let value = parse_transform_value(input)?;
        return Ok(vec![Declaration {
            name: name.to_string(),
            value,
            important: false,
        }]);
    }
    if name == "flex" {
        return parse_flex_shorthand(input);
    }
    if name == "background" {
        return parse_background_shorthand(input);
    }
    if name == "background-position" {
        return parse_background_position(input);
    }
    if name == "grid-template-columns" || name == "grid-template-rows" {
        let value = parse_grid_track_list(input)?;
        return Ok(vec![Declaration {
            name: name.to_string(),
            value,
            important: false,
        }]);
    }
    if name == "grid-column" || name == "grid-row" {
        let value = parse_grid_placement(input)?;
        return Ok(vec![Declaration {
            name: name.to_string(),
            value,
            important: false,
        }]);
    }
    if name == "grid-template-areas" {
        let value = parse_grid_template_areas(input)?;
        return Ok(vec![Declaration {
            name: name.to_string(),
            value,
            important: false,
        }]);
    }
    if name == "grid-template" {
        return parse_grid_template_shorthand(input);
    }

    let value = unitless_zero_as_length(name, parse_value(input)?);
    // Flexbox: negative flex factors are invalid (the declaration drops, as
    // in Chrome). A negative shrink weight would grow overflowing items.
    if matches!(name, "flex-grow" | "flex-shrink") && matches!(value, Value::Number(n) if n < 0.0) {
        return Err(ParseError::new(
            input.position().byte_index(),
            "negative flex factors are invalid",
        ));
    }
    Ok(vec![Declaration {
        name: name.to_string(),
        value,
        important: false,
    }])
}

/// How deeply a value may nest blocks, functions and `var()` fallbacks
/// before the engine gives up on it — the walks over them recurse, and a
/// stack overflow is an uncatchable abort (Robustness Rule 2). Past it a
/// declaration drops at parse time, or is invalid at computed-value time
/// during `var()` substitution (a custom-property chain counts each link).
/// Same as `media::MAX_MEDIA_NESTING`; real pages stay far below.
pub const MAX_VAR_NESTING: usize = MAX_MEDIA_NESTING;

/// Whether the value contains a `var()` (at any nesting depth); an error
/// for a malformed one — `var(` must start with a `--name`, optionally
/// followed by a comma and a fallback — or for nesting past
/// [`MAX_VAR_NESTING`], which drops the declaration.
fn scan_var_references<'i>(input: &mut CssParser<'i>) -> Result<bool, ParseError> {
    scan_var_references_at(input, 0)
}

fn scan_var_references_at<'i>(input: &mut CssParser<'i>, depth: usize) -> Result<bool, ParseError> {
    if depth > MAX_VAR_NESTING {
        return Err(ParseError::new(
            input.position().byte_index(),
            "value nested too deeply",
        ));
    }
    let mut found = false;
    loop {
        let token = match input.next() {
            Ok(token) => token.clone(),
            Err(_) => return Ok(found),
        };
        let nested = match token {
            cssparser::Token::Function(ref f) if f.eq_ignore_ascii_case("var") => {
                found = true;
                input
                    .parse_nested_block(|inner| {
                        check_var_arguments(inner, depth + 1).map_err(CssParseError::custom)
                    })
                    .map_err(error::convert_error)?;
                continue;
            }
            cssparser::Token::Function(_)
            | cssparser::Token::ParenthesisBlock
            | cssparser::Token::SquareBracketBlock
            | cssparser::Token::CurlyBracketBlock => input
                .parse_nested_block(|inner| {
                    scan_var_references_at(inner, depth + 1).map_err(CssParseError::custom)
                })
                .map_err(error::convert_error)?,
            _ => false,
        };
        found |= nested;
    }
}

fn check_var_arguments<'i>(input: &mut CssParser<'i>, depth: usize) -> Result<(), ParseError> {
    let position = input.position().byte_index();
    let name_ok =
        matches!(input.next(), Ok(cssparser::Token::Ident(name)) if name.starts_with("--"));
    if !name_ok {
        return Err(ParseError::new(
            position,
            "var() needs a --custom-property name",
        ));
    }
    if input.is_exhausted() {
        return Ok(());
    }
    if input.expect_comma().is_err() {
        return Err(ParseError::new(
            position,
            "var(): expected `,` before the fallback",
        ));
    }
    // The fallback is any tokens; nested `var()`s are checked too.
    scan_var_references_at(input, depth).map(|_| ())
}

/// The rest of the value as trimmed source text.
fn take_raw<'i>(input: &mut CssParser<'i>) -> String {
    let start = input.position();
    while input.next_including_whitespace_and_comments().is_ok() {}
    input.slice_from(start).trim().to_string()
}

/// `name: raw` kept for the cascade: a custom property as itself; any
/// other property on each longhand it sets (`longhands_of`), carrying the
/// shorthand to re-parse with.
fn unparsed_declarations(name: &str, raw: String) -> Vec<Declaration> {
    if name.starts_with("--") {
        return vec![Declaration {
            name: name.to_string(),
            value: Value::Unparsed {
                shorthand: None,
                raw,
            },
            important: false,
        }];
    }
    let longhands = longhands_of(name);
    let shorthand = (longhands.len() != 1 || longhands[0] != name).then(|| name.to_string());
    longhands
        .into_iter()
        .map(|longhand| Declaration {
            name: longhand,
            value: Value::Unparsed {
                shorthand: shorthand.clone(),
                raw: raw.clone(),
            },
            important: false,
        })
        .collect()
}

/// Parse `text` as the value of `name` (a property or a shorthand) — the
/// re-parse after `var()` substitution (step 1c). `None` when invalid.
pub fn parse_property_value(name: &str, text: &str) -> Option<Vec<Declaration>> {
    let mut parser = CssParser::new(text);
    parser
        .parse_entirely(|input| {
            parse_declaration_value(name, input).map_err(CssParseError::<CssError>::custom)
        })
        .ok()
}

/// `inherit | initial | unset | revert | revert-layer`, alone, lower-cased.
fn parse_css_wide_keyword<'i>(input: &mut CssParser<'i>) -> Result<String, ()> {
    let ident = input.expect_ident_cloned().map_err(|_| ())?;
    let lower = ident.to_ascii_lowercase();
    if !properties::CSS_WIDE_KEYWORDS.contains(&lower.as_str()) {
        return Err(());
    }
    input.skip_whitespace();
    input.expect_exhausted().map_err(|_| ())?;
    Ok(lower)
}

/// The longhands a shorthand sets, under the engine's keys; any other name
/// is its own single longhand.
fn longhands_of(name: &str) -> Vec<String> {
    const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];
    let each = |f: &dyn Fn(&str) -> String| SIDES.iter().map(|side| f(side)).collect();
    match name {
        "margin" | "padding" => each(&|side| format!("{name}-{side}")),
        "inset" => each(&|side| side.to_string()),
        "border" => SIDES
            .iter()
            .flat_map(|side| {
                [
                    format!("border-{side}"),
                    format!("border-{side}-style"),
                    format!("border-{side}-color"),
                ]
            })
            .collect(),
        "border-top" | "border-right" | "border-bottom" | "border-left" => {
            vec![
                name.to_string(),
                format!("{name}-style"),
                format!("{name}-color"),
            ]
        }
        "border-width" => each(&|side| format!("border-{side}")),
        "border-style" => each(&|side| format!("border-{side}-style")),
        "border-color" => each(&|side| format!("border-{side}-color")),
        "border-top-width" | "border-right-width" | "border-bottom-width" | "border-left-width" => {
            vec![name.trim_end_matches("-width").to_string()]
        }
        "border-radius" => [
            "border-top-left-radius",
            "border-top-right-radius",
            "border-bottom-right-radius",
            "border-bottom-left-radius",
        ]
        .map(String::from)
        .to_vec(),
        "flex" => ["flex-grow", "flex-shrink", "flex-basis"]
            .map(String::from)
            .to_vec(),
        "background" => [
            "background-color",
            "background-image",
            "background-position-x",
            "background-position-y",
            "background-size",
        ]
        .map(String::from)
        .to_vec(),
        "background-position" => ["background-position-x", "background-position-y"]
            .map(String::from)
            .to_vec(),
        "grid-template" => [
            "grid-template-rows",
            "grid-template-columns",
            "grid-template-areas",
        ]
        .map(String::from)
        .to_vec(),
        other => vec![other.to_string()],
    }
}

/// A bare `0` is a valid `<length>` (CSS Values 4 §6.1); for a length-valued
/// property store it as `0px` — layout and the taffy bridge read `Length`
/// only, so `top: 0` as `Number(0)` was silently ignored (step 1q).
/// `line-height` is length-kinded in the table but a unitless number there
/// is a multiplier; number-valued properties (`opacity`, `z-index`,
/// `flex-grow`…) aren't length-kinded and keep `Number`.
pub(crate) fn unitless_zero_as_length(name: &str, value: Value) -> Value {
    match value {
        Value::Number(n)
            if n == 0.0
                && name != "line-height"
                && matches!(
                    properties::lookup(name),
                    Some(properties::ValueKind::Length(_))
                ) =>
        {
            Value::Length(0.0, Unit::Px)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BackgroundSize, BoxShadow, ClipBox, ClipEdge, ClipFunction, Color, GradientDirection,
        GradientKind, GridLine, SizeAxis, Stylesheet, TextShadow, TrackSize, TransformOp, Unit,
        Value, parse,
    };

    /// `(name, value, important)` of every declaration in the sheet.
    fn flagged(css: &str) -> Vec<(String, Value, bool)> {
        parse(css)
            .unwrap()
            .rules
            .iter()
            .flat_map(|r| &r.declarations)
            .map(|d| (d.name.clone(), d.value.clone(), d.important))
            .collect()
    }

    #[test]
    fn important_declaration_is_kept_and_flagged() {
        let px = |v| Value::Length(v, Unit::Px);
        assert_eq!(
            flagged(".a{width:1px !important; height:2px}"),
            [
                ("width".to_string(), px(1.0), true),
                ("height".to_string(), px(2.0), false)
            ]
        );
        // Whitespace/comments between `!` and the word, any case.
        for css in [
            ".a{width:1px ! important}",
            ".a{width:1px!IMPORTANT}",
            ".a{width:1px !/**/important }",
        ] {
            assert_eq!(
                flagged(css),
                [("width".to_string(), px(1.0), true)],
                "{css}"
            );
        }
        let sheet = parse(".a{width:1px !important} .b{width:1px}").unwrap();
        assert!(sheet.rules[0].has_important);
        assert!(!sheet.rules[1].has_important);
    }

    #[test]
    fn important_garbage_suffix_drops() {
        // Anything after `!important`, or a `!` that isn't `!important`
        // (the old-IE `!ie` hack), drops the declaration — as in Chrome.
        let decls =
            flagged(".a{color:red !important foo; width:1px !ie; top:1px !imp; height:2px}");
        assert_eq!(
            decls,
            [("height".to_string(), Value::Length(2.0, Unit::Px), false)]
        );
    }

    #[test]
    fn custom_property_important_strips_flag() {
        let decls = flagged(":root{--x: 5px !important}");
        assert_eq!(
            decls,
            [(
                "--x".to_string(),
                Value::Unparsed {
                    shorthand: None,
                    raw: "5px".into()
                },
                true
            )]
        );
    }

    #[test]
    fn shorthand_longhands_inherit_important() {
        for css in [
            ".a{margin:1px 2px !important}",
            ".a{padding:1px !important}",
            ".a{border-radius:3px !important}",
            ".a{flex:1 !important}",
            ".a{background:red !important}",
        ] {
            let decls = flagged(css);
            assert!(!decls.is_empty(), "{css}");
            assert!(
                decls.iter().all(|(_, _, important)| *important),
                "{css}: {decls:?}"
            );
        }
    }

    #[test]
    fn important_inside_keyframes_is_ignored() {
        // @keyframes bodies aren't modelled; their `!important` (ignored by
        // spec) must not leak into normal rules.
        let sheet = parse("@keyframes k{from{color:red !important}to{color:blue}} .a{color:green}")
            .unwrap();
        assert_eq!(sheet.rules.len(), 1);
        assert!(!sheet.rules[0].has_important);
    }

    #[test]
    fn inline_style_important_is_flagged() {
        let decls = super::parse_inline_style("color: red !important; width: 1px");
        let flags: Vec<_> = decls
            .iter()
            .map(|d| (d.name.as_str(), d.important))
            .collect();
        assert_eq!(flags, [("color", true), ("width", false)]);
    }

    #[test]
    fn unitless_zero_is_a_px_length_for_length_properties() {
        // CSS allows a bare `0` wherever a length goes; layout and taffy only
        // read `Length`, so `top: 0` as `Number(0)` was ignored (step 1q).
        let decls = flagged(
            ".a{top:0;left:0;width:0;min-height:0;gap:0;text-indent:0;margin-top:0;flex-basis:0;\
             line-height:0;z-index:0;opacity:0;flex-grow:0}",
        );
        let value = |name: &str| decls.iter().find(|d| d.0 == name).unwrap().1.clone();
        for name in [
            "top",
            "left",
            "width",
            "min-height",
            "gap",
            "text-indent",
            "margin-top",
            "flex-basis",
        ] {
            assert_eq!(value(name), Value::Length(0.0, Unit::Px), "{name}");
        }
        // Number-valued (and line-height, where a number is a multiplier).
        for name in ["line-height", "z-index", "opacity", "flex-grow"] {
            assert_eq!(value(name), Value::Number(0.0), "{name}");
        }
        // Only zero: a unitless non-zero length stays invalid-ish as before.
        assert_eq!(flagged(".a{top:5}")[0].1, Value::Number(5.0));
    }

    // ---- 1b: value parsers (docs/compat-round3.md 1단계) ----

    /// Every declaration of `.a{…}` as `name → value`, in order (a later
    /// duplicate wins, like the cascade).
    fn decls(body: &str) -> std::collections::BTreeMap<String, Value> {
        flagged(&format!(".a{{{body}}}"))
            .into_iter()
            .map(|(name, value, _)| (name, value))
            .collect()
    }

    fn px(v: f32) -> Value {
        Value::Length(v, Unit::Px)
    }

    fn kw(k: &str) -> Value {
        Value::Keyword(k.into())
    }

    fn rgb(r: u8, g: u8, b: u8) -> Value {
        Value::Color(Color { r, g, b, a: 255 })
    }

    #[test]
    fn border_shorthand_expands_to_longhands() {
        let d = decls("border: 1px solid #ccc");
        for side in ["top", "right", "bottom", "left"] {
            assert_eq!(d[&format!("border-{side}")], px(1.0), "{side}");
            assert_eq!(d[&format!("border-{side}-style")], kw("solid"), "{side}");
            assert_eq!(
                d[&format!("border-{side}-color")],
                rgb(0xcc, 0xcc, 0xcc),
                "{side}"
            );
        }
        let d = decls("border-top: 2px dashed red");
        assert_eq!(d["border-top"], px(2.0));
        assert_eq!(d["border-top-style"], kw("dashed"));
        assert_eq!(d["border-top-color"], rgb(255, 0, 0));
        assert!(!d.contains_key("border-left"));
        // Any order; omitted parts reset to their initial values (medium /
        // none / currentcolor), as in Chrome.
        let d = decls("border-bottom: red solid");
        assert_eq!(d["border-bottom"], px(3.0));
        assert_eq!(d["border-bottom-style"], kw("solid"));
        let d = decls("border: 2px solid");
        assert_eq!(d["border-left-color"], kw("currentcolor"));
        let d = decls("border: none");
        assert_eq!(d["border-top-style"], kw("none"));
        let d = decls("border: 0");
        assert_eq!(d["border-top"], px(0.0));
        let d = decls("border-width: 1px 2px");
        assert_eq!(
            [
                &d["border-top"],
                &d["border-right"],
                &d["border-bottom"],
                &d["border-left"]
            ],
            [&px(1.0), &px(2.0), &px(1.0), &px(2.0)]
        );
        let d = decls("border-width: thin medium thick 0");
        assert_eq!(
            [
                &d["border-top"],
                &d["border-right"],
                &d["border-bottom"],
                &d["border-left"]
            ],
            [&px(1.0), &px(3.0), &px(5.0), &px(0.0)]
        );
        let d = decls("border-style: solid none");
        assert_eq!(
            (&d["border-top-style"], &d["border-left-style"]),
            (&kw("solid"), &kw("none"))
        );
        let d = decls("border-color: red blue");
        assert_eq!(
            (&d["border-top-color"], &d["border-right-color"]),
            (&rgb(255, 0, 0), &rgb(0, 0, 255))
        );
        // A real longhand width folds onto the existing key.
        let d = decls("border-left-width: 4px");
        assert_eq!(d["border-left"], px(4.0));
        // Junk drops the whole declaration.
        assert!(decls("border: 1px solid red blue").is_empty());
        assert!(decls("border: 1px 2px solid").is_empty());
    }

    #[test]
    fn font_family_list_parses() {
        let d = decls(r#"font-family: "Helvetica Neue", Arial, sans-serif"#);
        assert_eq!(
            d["font-family"],
            Value::FontFamilies(vec![
                "Helvetica Neue".into(),
                "Arial".into(),
                "sans-serif".into()
            ])
        );
        let d = decls("font-family: Apple SD Gothic Neo, monospace");
        assert_eq!(
            d["font-family"],
            Value::FontFamilies(vec!["Apple SD Gothic Neo".into(), "monospace".into()])
        );
        assert!(decls("font-family: Arial,, serif").is_empty());
    }

    #[test]
    fn box_shadow_list_keeps_first_layer() {
        let d = decls("box-shadow: 0 1px 2px red, 0 2px 4px rgba(0,0,0,.5)");
        match &d["box-shadow"] {
            Value::BoxShadow(BoxShadow {
                offset_y, color, ..
            }) => {
                assert_eq!((*offset_y, color.r), (1.0, 255));
            }
            other => panic!("{other:?}"),
        }
        // `inset` layers parse but aren't painted: the first outset one is.
        let d = decls("box-shadow: inset 0 1px red, 0 3px blue");
        assert!(matches!(&d["box-shadow"], Value::BoxShadow(s) if s.offset_y == 3.0));
        let d = decls("box-shadow: inset 0 1px red");
        assert_eq!(d["box-shadow"], kw("none"));
        assert_eq!(decls("box-shadow: none")["box-shadow"], kw("none"));
        assert!(decls("box-shadow: 0 1px red,").is_empty());
    }

    #[test]
    fn transition_parses_without_drop() {
        for (name, value) in [
            (
                "transition",
                "opacity .2s ease, transform .3s cubic-bezier(.2,0,0,1)",
            ),
            ("animation", "spin 1s linear infinite"),
            ("transition-property", "opacity, transform"),
            ("animation-timing-function", "steps(4, end)"),
        ] {
            let d = decls(&format!("{name}: {value}"));
            assert!(d.contains_key(name), "{name}: {d:?}");
        }
    }

    #[test]
    fn clip_rect_and_clip_path_inset_are_kept() {
        let rect = |top, right, bottom, left| {
            Value::ClipBox(ClipBox {
                function: ClipFunction::Rect,
                edges: [top, right, bottom, left],
            })
        };
        let l = |v| ClipEdge::Length(v, Unit::Px);
        assert_eq!(
            decls("clip: rect(0 0 0 0)")["clip"],
            rect(l(0.0), l(0.0), l(0.0), l(0.0))
        );
        assert_eq!(
            decls("clip: rect(1px, 1px, 1px, 1px)")["clip"],
            rect(l(1.0), l(1.0), l(1.0), l(1.0))
        );
        assert_eq!(
            decls("clip: rect(0, auto, 10px, auto)")["clip"],
            rect(l(0.0), ClipEdge::Auto, l(10.0), ClipEdge::Auto)
        );
        let inset = |v| ClipEdge::Length(v, Unit::Percent);
        assert_eq!(
            decls("clip-path: inset(50%)")["clip-path"],
            Value::ClipBox(ClipBox {
                function: ClipFunction::Inset,
                edges: [inset(50.0); 4]
            })
        );
        assert!(matches!(
            decls("clip-path: inset(0 0 0 0 round 4px)")["clip-path"],
            Value::ClipBox(_)
        ));
        assert_eq!(decls("clip: auto")["clip"], kw("auto"));
    }

    #[test]
    fn light_dark_picks_first() {
        assert_eq!(
            decls("color: light-dark(#fff, #000)")["color"],
            rgb(255, 255, 255)
        );
        assert_eq!(
            decls("background-color: light-dark(red, blue)")["background-color"],
            rgb(255, 0, 0)
        );
        assert!(decls("color: light-dark(#fff)").is_empty());
    }

    #[test]
    fn flex_shorthand_three_values_with_auto_basis() {
        let flex = |body: &str| {
            let d = decls(body);
            (
                d.get("flex-grow").cloned(),
                d.get("flex-shrink").cloned(),
                d.get("flex-basis").cloned(),
            )
        };
        let n = |v| Some(Value::Number(v));
        let pct = |v| Some(Value::Length(v, Unit::Percent));
        assert_eq!(flex("flex: 1 1 auto"), (n(1.0), n(1.0), Some(kw("auto"))));
        assert_eq!(flex("flex: 0 0 120px"), (n(0.0), n(0.0), Some(px(120.0))));
        assert_eq!(flex("flex: 1 1 0"), (n(1.0), n(1.0), Some(px(0.0))));
        assert_eq!(flex("flex: 1 1 0%"), (n(1.0), n(1.0), pct(0.0)));
        assert_eq!(
            flex("flex: 1"),
            (n(1.0), n(1.0), pct(0.0)),
            "<number> → basis 0%"
        );
        assert_eq!(flex("flex: 2 3"), (n(2.0), n(3.0), pct(0.0)));
        assert_eq!(flex("flex: 100px"), (n(1.0), n(1.0), Some(px(100.0))));
        assert_eq!(flex("flex: auto 2"), (n(2.0), n(1.0), Some(kw("auto"))));
        assert_eq!(flex("flex: none"), (n(0.0), n(0.0), Some(kw("auto"))));
        assert_eq!(flex("flex: auto"), (n(1.0), n(1.0), Some(kw("auto"))));
        // `initial` is a CSS-wide keyword: every longhand gets it and the
        // cascade resolves it to the initial `0 1 auto`.
        assert_eq!(
            flex("flex: initial"),
            (
                Some(kw("initial")),
                Some(kw("initial")),
                Some(kw("initial"))
            )
        );
        assert!(decls("flex: 1 2 3").is_empty());
    }

    #[test]
    fn background_size_two_lengths_and_keywords() {
        let size = |body: &str| decls(body)["background-size"].clone();
        let l = |v| SizeAxis::Length(v, Unit::Px);
        assert_eq!(
            size("background-size: 224px 242px"),
            Value::BackgroundSize(BackgroundSize::Size(l(224.0), l(242.0)))
        );
        assert_eq!(
            size("background-size: cover"),
            Value::BackgroundSize(BackgroundSize::Cover)
        );
        assert_eq!(
            size("background-size: contain"),
            Value::BackgroundSize(BackgroundSize::Contain)
        );
        assert_eq!(
            size("background-size: auto"),
            Value::BackgroundSize(BackgroundSize::Size(SizeAxis::Auto, SizeAxis::Auto))
        );
        assert_eq!(
            size("background-size: 50%"),
            Value::BackgroundSize(BackgroundSize::Size(
                SizeAxis::Length(50.0, Unit::Percent),
                SizeAxis::Auto
            ))
        );
        assert_eq!(
            size("background-size: auto 0"),
            Value::BackgroundSize(BackgroundSize::Size(SizeAxis::Auto, l(0.0)))
        );
        // The shorthand's `<position> / <size>` and its position.
        let d = decls("background: url(s.png) no-repeat -10px -20px / 224px 242px");
        assert_eq!(
            d["background-size"],
            Value::BackgroundSize(BackgroundSize::Size(l(224.0), l(242.0)))
        );
        assert_eq!(
            (&d["background-position-x"], &d["background-position-y"]),
            (&px(-10.0), &px(-20.0))
        );
        assert!(matches!(d["background-image"], Value::ImageUrl(_)));
        assert!(decls("background-size: 1px 2px 3px").is_empty());
    }

    #[test]
    fn inset_shorthand_expands_to_offsets() {
        let d = decls("inset: 0");
        for side in ["top", "right", "bottom", "left"] {
            assert_eq!(d[side], px(0.0), "{side}");
        }
        let d = decls("inset: 1px auto");
        assert_eq!(
            (&d["top"], &d["right"], &d["bottom"], &d["left"]),
            (&px(1.0), &kw("auto"), &px(1.0), &kw("auto"))
        );
    }

    #[test]
    fn global_keywords_and_all_are_kept() {
        for value in ["inherit", "initial", "unset", "revert", "revert-layer"] {
            assert_eq!(decls(&format!("color: {value}"))["color"], kw(value));
            assert_eq!(
                decls(&format!("border: {value}"))["border-top"],
                kw(value),
                "shorthands take them too"
            );
            assert_eq!(decls(&format!("all: {value}"))["all"], kw(value));
        }
        assert!(decls("all: red").is_empty());
    }

    #[test]
    fn negative_flex_factors_are_invalid_and_dropped() {
        // CSS Flexbox: "Negative numbers are invalid" for flex-grow and
        // flex-shrink, so Chrome drops the declaration (an earlier valid one
        // still applies). Kept, a negative shrink weight *grows* overflowing
        // items — the fuzzer saw 1.7e7 px from `flex-shrink: -1`.
        for css in [
            "a{flex-shrink:-1}",
            "a{flex-grow:-0.5}",
            "a{flex:-1}",
            "a{flex:1 -1}",
            "a{flex:1 -1 0px}",
        ] {
            let sheet = parse(css).expect("sheet parses");
            let decls: Vec<_> = sheet.rules.iter().flat_map(|r| &r.declarations).collect();
            assert!(decls.is_empty(), "{css:?}: {decls:?}");
        }
        let sheet = parse("a{flex-shrink:2; flex-shrink:-1; flex: 0 3}").unwrap();
        let decls = &sheet.rules[0].declarations;
        assert_eq!(decls[0].value, Value::Number(2.0));
        assert_eq!(
            decls.len(),
            4,
            "valid declarations stay (flex: grow, shrink, basis): {decls:?}"
        );
    }

    #[test]
    fn non_ascii_hash_color_drops_the_declaration_not_a_panic() {
        // A `#` token's value is an identifier, so it may hold multibyte
        // characters. Three or six bytes of those used to index past the
        // char list or slice inside a character.
        for css in [
            "a{color:#\u{e9}1}",
            "a{color:#\u{d55c}\u{ae00}}",
            "a{color:#ab\u{20ac}}",
        ] {
            let sheet = parse(css).expect("sheet parses");
            let decls: Vec<_> = sheet.rules.iter().flat_map(|r| &r.declarations).collect();
            assert!(
                decls.iter().all(|d| !matches!(d.value, Value::Color(_))),
                "{css:?}: {decls:?}"
            );
        }
    }

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
        assert_eq!(stylesheet.rules[1].selectors.specificity(), 2 * class_unit);
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
        assert_eq!(
            stylesheet.rules[0].selectors.specificity(),
            3 * element_unit
        );
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
        // Phase 6.K expands `padding` into the four per-side longhands,
        // so look up properties by name rather than by index.
        let by_name = |target: &str| {
            decls
                .iter()
                .find(|d| d.name == target)
                .unwrap_or_else(|| panic!("missing declaration {target}"))
        };
        assert_eq!(by_name("width").value, Value::Length(50.0, Unit::Percent));
        assert_eq!(by_name("padding-top").value, Value::Length(1.5, Unit::Em));
        assert_eq!(by_name("padding-right").value, Value::Length(1.5, Unit::Em));
        assert_eq!(
            by_name("padding-bottom").value,
            Value::Length(1.5, Unit::Em)
        );
        assert_eq!(by_name("padding-left").value, Value::Length(1.5, Unit::Em));
        assert_eq!(by_name("font-size").value, Value::Length(0.875, Unit::Rem));
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
        let stylesheet = parse(r#".g { grid-template-areas: "h h h" "s m m" "f f f"; }"#).unwrap();
        match &stylesheet.rules[0].declarations[0].value {
            Value::TemplateAreas(rows) => {
                assert_eq!(rows.len(), 3);
                assert_eq!(
                    rows[0],
                    vec![Some("h".into()), Some("h".into()), Some("h".into())]
                );
                assert_eq!(
                    rows[1],
                    vec![Some("s".into()), Some("m".into()), Some("m".into())]
                );
                assert_eq!(
                    rows[2],
                    vec![Some("f".into()), Some("f".into()), Some("f".into())]
                );
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
        // `flex: <number>` = `<number> 1 0%` (step 1b: the basis was left
        // unset — auto — before).
        let stylesheet = parse(".a { flex: 2; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;
        assert_eq!(decls.len(), 3);
        assert_eq!(decls[0].name, "flex-grow");
        assert_eq!(decls[0].value, Value::Number(2.0));
        assert_eq!(decls[1].name, "flex-shrink");
        assert_eq!(decls[1].value, Value::Number(1.0));
        assert_eq!(decls[2].value, Value::Length(0.0, Unit::Percent));
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

    fn decl<'a>(sheet: &'a Stylesheet, name: &str) -> Option<&'a Value> {
        sheet.rules[0]
            .declarations
            .iter()
            .find(|d| d.name == name)
            .map(|d| &d.value)
    }

    #[test]
    fn grid_track_list_parses_minmax_and_content_keywords() {
        use super::TrackBreadth as B;
        let sheet = parse(
            ".g { grid-template-columns: minmax(0,1fr) min-content max-content minmax(100px, 59.25rem) minmax(min-content, auto); }",
        )
        .unwrap();
        assert_eq!(
            decl(&sheet, "grid-template-columns"),
            Some(&Value::TrackList(vec![
                TrackSize::MinMax(B::Length(0.0, Unit::Px), B::Fraction(1.0)),
                TrackSize::MinContent,
                TrackSize::MaxContent,
                TrackSize::MinMax(B::Length(100.0, Unit::Px), B::Length(59.25, Unit::Rem)),
                TrackSize::MinMax(B::MinContent, B::Auto),
            ]))
        );
    }

    #[test]
    fn grid_minmax_rejects_a_flexible_minimum_and_bad_arity() {
        // `fr` is not a valid minimum (css-grid-1 §7.2.3); the whole
        // declaration is invalid, like in a browser.
        for bad in [
            "minmax(1fr, 10px)",
            "minmax(10px)",
            "minmax(1px, 2px, 3px)",
            "minmax(5, 1fr)",
        ] {
            let sheet = parse(&format!(".g {{ grid-template-columns: {bad}; }}")).unwrap();
            assert_eq!(decl(&sheet, "grid-template-columns"), None, "{bad}");
        }
    }

    #[test]
    fn grid_template_none_keywords_parse() {
        let sheet =
            parse(".g { grid-template-areas: none; grid-template-columns: none; }").unwrap();
        assert_eq!(
            decl(&sheet, "grid-template-areas"),
            Some(&Value::Keyword("none".into()))
        );
        assert_eq!(
            decl(&sheet, "grid-template-columns"),
            Some(&Value::Keyword("none".into()))
        );
    }

    #[test]
    fn grid_template_shorthand_rows_slash_columns() {
        // Vector 2022 (Wikipedia): `grid-template: <rows> / <columns>`.
        // The shorthand also resets `grid-template-areas` to `none`.
        let sheet =
            parse(".g { grid-template: min-content 1fr min-content / 12.25rem minmax(0,1fr); }")
                .unwrap();
        assert_eq!(
            decl(&sheet, "grid-template-rows"),
            Some(&Value::TrackList(vec![
                TrackSize::MinContent,
                TrackSize::Fraction(1.0),
                TrackSize::MinContent
            ]))
        );
        assert_eq!(
            decl(&sheet, "grid-template-columns"),
            Some(&Value::TrackList(vec![
                TrackSize::Length(12.25, Unit::Rem),
                TrackSize::MinMax(
                    super::TrackBreadth::Length(0.0, Unit::Px),
                    super::TrackBreadth::Fraction(1.0)
                ),
            ]))
        );
        assert_eq!(
            decl(&sheet, "grid-template-areas"),
            Some(&Value::Keyword("none".into()))
        );
    }

    #[test]
    fn grid_template_shorthand_areas_form() {
        // `[ <string> <track-size>? ]+ [ / <track-list> ]?`
        let sheet = parse(r#".g { grid-template: "a b" 40px 'c d' / 1fr 2fr; }"#).unwrap();
        assert_eq!(
            decl(&sheet, "grid-template-areas"),
            Some(&Value::TemplateAreas(vec![
                vec![Some("a".into()), Some("b".into())],
                vec![Some("c".into()), Some("d".into())],
            ]))
        );
        assert_eq!(
            decl(&sheet, "grid-template-rows"),
            Some(&Value::TrackList(vec![
                TrackSize::Length(40.0, Unit::Px),
                TrackSize::Auto
            ]))
        );
        assert_eq!(
            decl(&sheet, "grid-template-columns"),
            Some(&Value::TrackList(vec![
                TrackSize::Fraction(1.0),
                TrackSize::Fraction(2.0)
            ]))
        );
        // Without columns: rows only, columns reset to none.
        let sheet = parse(r#".g { grid-template: "a" "b"; }"#).unwrap();
        assert_eq!(
            decl(&sheet, "grid-template-columns"),
            Some(&Value::Keyword("none".into()))
        );
    }

    #[test]
    fn grid_template_shorthand_rejects_garbage_whole() {
        for bad in [
            "/ 1fr",
            "1fr /",
            "1fr / 1fr / 1fr",
            "\"a\" 1fr 2fr",
            "1fr \"a\"",
        ] {
            let sheet = parse(&format!(".g {{ grid-template: {bad}; }}")).unwrap();
            assert!(sheet.rules[0].declarations.is_empty(), "{bad}");
        }
        let sheet = parse(".g { grid-template: none; }").unwrap();
        assert_eq!(sheet.rules[0].declarations.len(), 3);
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

    #[test]
    fn background_shorthand_with_url_and_repeat_keyword_expands_to_image_longhand() {
        // Phase 5.6: HN's `.votearrow { background: url(grayarrow.gif)
        // no-repeat; }` shape. Pre-5.6 the trailing `no-repeat` token
        // tripped the generic parse_value path so the entire declaration
        // got dropped, which in turn meant the resource fetcher and the
        // painter never saw the URL. The longhand expansion is what
        // lands the value where the rest of the pipeline expects it.
        let stylesheet = parse(r#".vote { background: url("grayarrow.gif") no-repeat; }"#).unwrap();
        let decls = &stylesheet.rules[0].declarations;
        let image = decls
            .iter()
            .find(|d| d.name == "background-image")
            .expect("image longhand missing");
        assert_eq!(image.value, Value::ImageUrl("grayarrow.gif".to_string()),);
        // The trailing `no-repeat` keyword is silently discarded — the
        // toy renderer only paints non-tiled bg images today, so
        // splitting it into its own longhand would be dead plumbing
        // that future work would just delete.
        assert!(
            !decls.iter().any(|d| d.name == "background-repeat"),
            "no-repeat keyword should not synthesise a background-repeat longhand yet",
        );
    }

    #[test]
    fn background_shorthand_with_color_and_url_emits_both_longhands() {
        // Order-insensitive: token role is decided by value shape (Color
        // vs ImageUrl), not position. A page that authors the URL first
        // would also work — important because real CSS authors mix the
        // order freely.
        let stylesheet =
            parse(r#".panel { background: #ff0000 url("foo.png") no-repeat; }"#).unwrap();
        let decls = &stylesheet.rules[0].declarations;
        let color = decls
            .iter()
            .find(|d| d.name == "background-color")
            .expect("color longhand missing");
        let image = decls
            .iter()
            .find(|d| d.name == "background-image")
            .expect("image longhand missing");
        assert_eq!(
            color.value,
            Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }),
        );
        assert_eq!(image.value, Value::ImageUrl("foo.png".to_string()));
    }

    #[test]
    fn background_shorthand_with_color_only_emits_background_color() {
        // Plain `background: red` — used by pages that want a flat
        // panel without an image. The shorthand walker must handle this
        // without insisting on an image too, otherwise legacy pages
        // that wrote bg colors via the shorthand would lose them.
        // The shorthand also resets the image to `none` (step 1b).
        let stylesheet = parse(r#".panel { background: red; }"#).unwrap();
        let decls = &stylesheet.rules[0].declarations;
        assert_eq!(decls.len(), 2);
        assert_eq!(decls[0].name, "background-color");
        assert!(matches!(decls[0].value, Value::Color(_)));
        assert_eq!(decls[1].value, Value::Keyword("none".into()));
    }

    #[test]
    fn parses_ch_length_unit() {
        // `65ch` is the canonical reading-width unit pages use for body
        // copy. The parser preserves the raw value + unit; cascade later
        // converts it to Px against the element's resolved font-size.
        let stylesheet = parse(".article { max-width: 65ch; }").unwrap();
        let value = &stylesheet.rules[0].declarations[0].value;
        assert_eq!(*value, Value::Length(65.0, Unit::Ch));
    }

    #[test]
    fn parses_background_position_into_two_longhands() {
        // Phase 6.G: HN's vote arrow uses `background-position: 0 -10px`
        // to slice a vertical sprite strip. The parser splits the
        // 2-value form into the per-axis longhands `background-position-x`
        // / `-y` so the renderer can read them independently at paint
        // time without reparsing the shorthand.
        let stylesheet = parse(".vote { background-position: -10px -20px; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;
        let x = decls
            .iter()
            .find(|d| d.name == "background-position-x")
            .expect("x longhand");
        let y = decls
            .iter()
            .find(|d| d.name == "background-position-y")
            .expect("y longhand");
        assert_eq!(x.value, Value::Length(-10.0, Unit::Px));
        assert_eq!(y.value, Value::Length(-20.0, Unit::Px));
    }

    #[test]
    fn background_position_omitted_y_falls_back_to_zero() {
        // Single-value form (only x) — HN actually uses `0 -10px` so the
        // 2-value path is the hot one, but stray inputs like
        // `background-position: 50%` shouldn't error out the cascade.
        let stylesheet = parse(".vote { background-position: 5px; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;
        let x = decls
            .iter()
            .find(|d| d.name == "background-position-x")
            .expect("x longhand");
        let y = decls
            .iter()
            .find(|d| d.name == "background-position-y")
            .expect("y longhand");
        assert_eq!(x.value, Value::Length(5.0, Unit::Px));
        assert_eq!(y.value, Value::Length(0.0, Unit::Px));
    }

    #[test]
    fn padding_shorthand_expands_to_four_longhands_clockwise() {
        // Phase 6.K: HN's orange header uses `<table style="padding:2px">`,
        // which never reached layout before because `padding` only landed
        // under the literal shorthand key. The cascade reads
        // `padding-{top|right|bottom|left}`, so the shorthand has to
        // expand at parse time.
        let stylesheet = parse(".hd { padding: 1px 2px 3px 4px; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;
        let by_name = |target: &str| {
            decls
                .iter()
                .find(|d| d.name == target)
                .unwrap_or_else(|| panic!("missing {target}"))
                .value
                .clone()
        };
        assert_eq!(by_name("padding-top"), Value::Length(1.0, Unit::Px));
        assert_eq!(by_name("padding-right"), Value::Length(2.0, Unit::Px));
        assert_eq!(by_name("padding-bottom"), Value::Length(3.0, Unit::Px));
        assert_eq!(by_name("padding-left"), Value::Length(4.0, Unit::Px));
    }

    #[test]
    fn padding_single_value_expands_uniformly() {
        // The HN orange-header form: `padding:2px` (no spaces) — every
        // side ends up at the same length.
        let stylesheet = parse(".hd { padding: 2px; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;
        for side in ["top", "right", "bottom", "left"] {
            let target = format!("padding-{side}");
            let decl = decls
                .iter()
                .find(|d| d.name == target)
                .unwrap_or_else(|| panic!("missing {target}"));
            assert_eq!(decl.value, Value::Length(2.0, Unit::Px));
        }
    }

    #[test]
    fn margin_shorthand_accepts_auto_keyword_for_horizontal_centering() {
        // `margin: 0 auto` is the canonical centering trick; `auto` must
        // round-trip as a Keyword so layout's auto-margin path picks it
        // up, while the numeric sides stay as Length.
        let stylesheet = parse(".centered { margin: 0 auto; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;
        let by_name = |target: &str| {
            decls
                .iter()
                .find(|d| d.name == target)
                .unwrap_or_else(|| panic!("missing {target}"))
                .value
                .clone()
        };
        assert_eq!(by_name("margin-top"), Value::Length(0.0, Unit::Px));
        assert_eq!(by_name("margin-right"), Value::Keyword("auto".into()));
        assert_eq!(by_name("margin-bottom"), Value::Length(0.0, Unit::Px));
        assert_eq!(by_name("margin-left"), Value::Keyword("auto".into()));
    }

    #[test]
    fn media_block_is_shared_by_its_rules_and_nested_blocks_chain() {
        let sheet =
            parse(".top{} @media screen { .a{} .b{} @media (min-width: 1px) { .c{} } .d{} } .e{}")
                .unwrap();
        let media: Vec<_> = sheet
            .rules
            .iter()
            .map(|rule| rule.media.as_ref().map(|block| block.index))
            .collect();
        assert_eq!(media, [None, Some(0), Some(0), Some(1), Some(0), None]);
        assert!(std::rc::Rc::ptr_eq(
            sheet.rules[1].media.as_ref().unwrap(),
            sheet.rules[2].media.as_ref().unwrap()
        ));
        assert_eq!(sheet.media.len(), 2);
        let parent = sheet.media[1].parent.as_ref().expect("nested block");
        assert!(std::rc::Rc::ptr_eq(parent, &sheet.media[0]));
    }

    #[test]
    fn deeply_nested_media_blocks_are_bounded() {
        // 10,000 nested blocks must neither overflow the stack nor keep
        // rules past the cap; what follows the nest still parses.
        let depth = 10_000;
        let css = format!(
            "{}.deep{{color:red}}{} .after{{color:blue}}",
            "@media screen{".repeat(depth),
            "}".repeat(depth)
        );
        let sheet = parse(&css).unwrap();
        assert_eq!(sheet.media.len(), super::media::MAX_MEDIA_NESTING);
        assert_eq!(
            sheet.rules.len(),
            1,
            "only .after survives: {:?}",
            sheet.rules.len()
        );
        assert!(sheet.rules[0].media.is_none());
    }

    #[test]
    fn media_rule_unfolds_inner_rules_into_top_level_stylesheet() {
        // Phase 6.F: `@media` blocks unfold into the top-level rule list;
        // since compat round 3 2.1 each rule also carries its block, which
        // the cascade evaluates against the viewport.
        let stylesheet = parse(
            r#"
                .foo { color: blue; }
                @media (min-width: 768px) {
                    .foo { color: red; }
                    .bar { display: flex; }
                }
                .baz { font-size: 14px; }
            "#,
        )
        .unwrap();

        assert_eq!(stylesheet.rules.len(), 4);
        // Final cascade order: .foo (mobile) then .foo (desktop), the
        // selectors-crate sort will pick the later one. Source-order
        // here verifies the @media block kept its inner rules.
        let names: Vec<_> = stylesheet
            .rules
            .iter()
            .flat_map(|r| r.declarations.iter().map(|d| d.name.as_str()))
            .collect();
        assert!(names.contains(&"display"), "@media inner rule lost");
    }

    #[test]
    fn unknown_at_rules_are_skipped_without_error() {
        // `@charset` and `@font-face` aren't modeled; the parser must
        // walk past them and keep returning the surrounding qualified
        // rules. Pre-Phase 6.F this was the at-rule behaviour for
        // every variant, including @media — Phase 6.F only carves out
        // @media as a recognised at-rule.
        let stylesheet = parse(
            r#"
                @charset "utf-8";
                @font-face { font-family: "X"; src: url("x.woff"); }
                .foo { color: red; }
            "#,
        )
        .unwrap();

        assert_eq!(stylesheet.rules.len(), 1);
        assert_eq!(stylesheet.rules[0].declarations[0].name, "color");
    }

    #[test]
    fn parses_pt_length_unit() {
        // `10pt` is HN's body font-size; before Phase 6.A the parser fell
        // through to `Keyword("10pt")` and the cascade defaulted body to 16px,
        // making text look 25% small. Now it round-trips as Length(_, Pt) so
        // the cascade can scale it to 13.33px (10 × 4/3).
        let stylesheet = parse(".body { font-size: 10pt; }").unwrap();
        let value = &stylesheet.rules[0].declarations[0].value;
        assert_eq!(*value, Value::Length(10.0, Unit::Pt));
    }

    #[test]
    fn parses_custom_property_declaration() {
        // A custom property is a token sequence, kept as source text (step
        // 1c) — what it means depends on where it is substituted.
        let stylesheet = parse(":root { --accent: #ff0000; }").unwrap();
        let decl = &stylesheet.rules[0].declarations[0];
        assert_eq!(decl.name, "--accent");
        assert_eq!(decl.value, unparsed(None, "#ff0000"));
    }

    fn unparsed(shorthand: Option<&str>, raw: &str) -> Value {
        Value::Unparsed {
            shorthand: shorthand.map(String::from),
            raw: raw.into(),
        }
    }

    #[test]
    fn parses_var_reference_with_no_fallback() {
        // Step 1c: any `var(` keeps the value unparsed until the cascade.
        let stylesheet = parse(".a { color: var(--accent); }").unwrap();
        let value = &stylesheet.rules[0].declarations[0].value;
        assert_eq!(value, &unparsed(None, "var(--accent)"));
    }

    #[test]
    fn parses_var_reference_with_color_fallback() {
        let stylesheet = parse(".a { color: var(--accent, #00ff00); }").unwrap();
        let value = &stylesheet.rules[0].declarations[0].value;
        assert_eq!(value, &unparsed(None, "var(--accent, #00ff00)"));
    }

    #[test]
    fn custom_property_keeps_raw_tokens() {
        let d = decls("--x: 255,255,255; --y: 0 5px 2px rgba(0,0,0,.2); --e:; --z: red !important");
        assert_eq!(d["--x"], unparsed(None, "255,255,255"));
        assert_eq!(d["--y"], unparsed(None, "0 5px 2px rgba(0,0,0,.2)"));
        assert_eq!(
            d["--e"],
            unparsed(None, ""),
            "an empty custom property is valid"
        );
        assert_eq!(
            d["--z"],
            unparsed(None, "red"),
            "the !important flag is not part of the value"
        );
        let flags = flagged(".a{--z: red !important}");
        assert!(flags[0].2);
    }

    #[test]
    fn var_values_stay_unparsed_and_shorthands_expand_per_longhand() {
        let d = decls("color: rgb(var(--x)); padding: 5px var(--p); border-top-width: var(--w)");
        assert_eq!(d["color"], unparsed(None, "rgb(var(--x))"));
        for side in ["top", "right", "bottom", "left"] {
            assert_eq!(
                d[&format!("padding-{side}")],
                unparsed(Some("padding"), "5px var(--p)")
            );
        }
        // A name the engine stores under another key re-parses through it.
        assert_eq!(
            d["border-top"],
            unparsed(Some("border-top-width"), "var(--w)")
        );
        // `var(` anywhere, even nested in a function or a fallback.
        let d = decls("width: calc(var(--a) + 1px)");
        assert_eq!(d["width"], unparsed(None, "calc(var(--a) + 1px)"));
        // A broken `var()` (not a custom name) drops the declaration.
        assert!(decls("color: var(accent)").is_empty());
    }
}
