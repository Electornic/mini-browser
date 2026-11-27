//! `@media` queries: parse once, evaluate against the viewport
//! (docs/compat-round3.md 2.1).
//!
//! The evaluation input is only [`MediaContext`] — the viewport in CSS px.
//! Everything else is a constant of the agent's browser: a light-theme
//! desktop screen with a fine pointer that hovers, 1dppx, scripting on.
//! That keeps observations deterministic (AGENTS.md "결정성") — the
//! watching window's Retina scale or OS theme never reach the page.
//!
//! Media Queries 4 three-valued logic: an unknown feature, a known feature
//! with a value it doesn't take, or any other `( … )` we can't read is
//! *unknown*, which `not` keeps unknown and a query treats as false. A
//! query that doesn't parse at all is `not all` — only that comma item.
//!
//! Everything here is bounded: parentheses deeper than
//! [`MAX_MEDIA_NESTING`] read as unknown, and the stylesheet parser stops
//! descending into `@media` blocks at the same depth.

use std::borrow::Cow;
use std::rc::Rc;

use cssparser::{Delimiter, ParseError, Parser, Token};

/// Deepest `( … )` nesting inside one query, and deepest `@media` block
/// nesting in a sheet. Real pages use 1–3; the cap keeps a hostile
/// `((((…` or `@media{@media{…` from recursing without bound.
pub const MAX_MEDIA_NESTING: usize = 32;

/// Media queries' initial font size (`em`/`rem` in a query are always 16px).
const INITIAL_FONT_SIZE: f32 = 16.0;

/// What a query is evaluated against: the page viewport, in CSS px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MediaContext {
    pub viewport_width: f32,
    pub viewport_height: f32,
}

impl MediaContext {
    /// The agent's default viewport (`AgentConfig` 1280×800).
    pub const DESKTOP: MediaContext = MediaContext::new(1280.0, 800.0);

    pub const fn new(viewport_width: f32, viewport_height: f32) -> Self {
        Self {
            viewport_width,
            viewport_height,
        }
    }
}

impl Default for MediaContext {
    fn default() -> Self {
        Self::DESKTOP
    }
}

/// A comma-separated media query list. Matches when any query does; an
/// empty list (`@media {}`, `media=""`) matches everything.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaQueryList {
    pub queries: Vec<MediaQuery>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MediaQuery {
    /// A comma item that didn't parse: `not all`.
    Invalid,
    Query {
        /// Leading `not`: negates the whole query (an unknown result stays
        /// false).
        negated: bool,
        media_type: MediaType,
        condition: Option<MediaCondition>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    All,
    Screen,
    /// `print`, `speech`, the deprecated types (`tv`, `handheld`…) and any
    /// unknown name — valid, matches nothing here.
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MediaCondition {
    Feature(MediaFeature),
    Not(Box<MediaCondition>),
    And(Vec<MediaCondition>),
    Or(Vec<MediaCondition>),
    /// `<general-enclosed>`: an unknown feature, a value the feature
    /// doesn't take, a function, or nesting past [`MAX_MEDIA_NESTING`].
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MediaFeature {
    /// A range feature compared against values, all of which must hold
    /// (`min-width: 1px` → `[≥ 1px]`, `400px <= width <= 700px` → two).
    Range(RangeFeature, Vec<(Comparison, MediaValue)>),
    /// A range feature in boolean context (`(color)`): true when not 0.
    RangeBoolean(RangeFeature),
    /// A discrete feature; `None` = boolean context (`(hover)`).
    Discrete(&'static DiscreteFeature, Option<&'static str>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeFeature {
    Width,
    Height,
    AspectRatio,
    /// `device-*` (deprecated): the viewport — the agent's "device" is its
    /// viewport, so they agree with `width`/`height`.
    DeviceWidth,
    DeviceHeight,
    DeviceAspectRatio,
    Resolution,
    /// `-webkit-device-pixel-ratio` (a plain number of dppx).
    DevicePixelRatio,
    Color,
    ColorIndex,
    Monochrome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueKind {
    Length,
    Ratio,
    Resolution,
    Integer,
    Number,
}

impl RangeFeature {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "width" => Self::Width,
            "height" => Self::Height,
            "aspect-ratio" => Self::AspectRatio,
            "device-width" => Self::DeviceWidth,
            "device-height" => Self::DeviceHeight,
            "device-aspect-ratio" => Self::DeviceAspectRatio,
            "resolution" => Self::Resolution,
            "color" => Self::Color,
            "color-index" => Self::ColorIndex,
            "monochrome" => Self::Monochrome,
            _ => return None,
        })
    }

    fn kind(self) -> ValueKind {
        match self {
            Self::Width | Self::Height | Self::DeviceWidth | Self::DeviceHeight => {
                ValueKind::Length
            }
            Self::AspectRatio | Self::DeviceAspectRatio => ValueKind::Ratio,
            Self::Resolution => ValueKind::Resolution,
            Self::DevicePixelRatio => ValueKind::Number,
            Self::Color | Self::ColorIndex | Self::Monochrome => ValueKind::Integer,
        }
    }

    fn actual(self, ctx: &MediaContext) -> f32 {
        match self {
            Self::Width | Self::DeviceWidth => ctx.viewport_width,
            Self::Height | Self::DeviceHeight => ctx.viewport_height,
            Self::AspectRatio | Self::DeviceAspectRatio => {
                ratio(ctx.viewport_width, ctx.viewport_height)
            }
            // 1dppx whatever the screen: the agent's pixels are CSS px.
            Self::Resolution | Self::DevicePixelRatio => 1.0,
            // 8 bits per colour component, not a palette, not monochrome.
            Self::Color => 8.0,
            Self::ColorIndex | Self::Monochrome => 0.0,
        }
    }
}

/// A discrete feature: the values it accepts, the agent's value, and the
/// value that reads as false in boolean context (`(hover)`).
#[derive(Debug, PartialEq, Eq)]
pub struct DiscreteFeature {
    pub name: &'static str,
    values: &'static [&'static str],
    current: Current,
    false_value: Option<&'static str>,
}

#[derive(Debug, PartialEq, Eq)]
enum Current {
    Fixed(&'static str),
    Orientation,
}

const fn fixed(
    name: &'static str,
    values: &'static [&'static str],
    current: &'static str,
    false_value: Option<&'static str>,
) -> DiscreteFeature {
    DiscreteFeature {
        name,
        values,
        current: Current::Fixed(current),
        false_value,
    }
}

/// Every discrete feature we know, with the agent's (desktop, light) value.
/// Anything else — `-ms-high-contrast`, `prefers-reduced-data`, typos — is
/// unknown, so false even under `not`.
static DISCRETE: &[DiscreteFeature] = &[
    DiscreteFeature {
        name: "orientation",
        values: &["portrait", "landscape"],
        current: Current::Orientation,
        false_value: None,
    },
    // Light: the same reason agent screenshots are light.
    fixed("prefers-color-scheme", &["light", "dark"], "light", None),
    fixed(
        "prefers-reduced-motion",
        &["no-preference", "reduce"],
        "no-preference",
        Some("no-preference"),
    ),
    fixed(
        "prefers-reduced-transparency",
        &["no-preference", "reduce"],
        "no-preference",
        Some("no-preference"),
    ),
    fixed(
        "prefers-contrast",
        &["no-preference", "more", "less", "custom"],
        "no-preference",
        Some("no-preference"),
    ),
    fixed("forced-colors", &["none", "active"], "none", Some("none")),
    fixed(
        "inverted-colors",
        &["none", "inverted"],
        "none",
        Some("none"),
    ),
    fixed("hover", &["none", "hover"], "hover", Some("none")),
    fixed("any-hover", &["none", "hover"], "hover", Some("none")),
    fixed("pointer", &["none", "coarse", "fine"], "fine", Some("none")),
    fixed(
        "any-pointer",
        &["none", "coarse", "fine"],
        "fine",
        Some("none"),
    ),
    // The page's scripts run.
    fixed(
        "scripting",
        &["none", "initial-only", "enabled"],
        "enabled",
        Some("none"),
    ),
    fixed(
        "display-mode",
        &[
            "fullscreen",
            "standalone",
            "minimal-ui",
            "browser",
            "picture-in-picture",
            "window-controls-overlay",
        ],
        "browser",
        None,
    ),
    fixed("color-gamut", &["srgb", "p3", "rec2020"], "srgb", None),
    fixed("dynamic-range", &["standard", "high"], "standard", None),
    fixed(
        "video-dynamic-range",
        &["standard", "high"],
        "standard",
        None,
    ),
    fixed("update", &["none", "slow", "fast"], "fast", Some("none")),
    fixed(
        "overflow-block",
        &["none", "scroll", "paged"],
        "scroll",
        Some("none"),
    ),
    fixed(
        "overflow-inline",
        &["none", "scroll"],
        "scroll",
        Some("none"),
    ),
];

impl DiscreteFeature {
    fn lookup(name: &str) -> Option<&'static DiscreteFeature> {
        DISCRETE.iter().find(|feature| feature.name == name)
    }

    fn current(&self, ctx: &MediaContext) -> &'static str {
        match self.current {
            Current::Fixed(value) => value,
            Current::Orientation if ctx.viewport_height >= ctx.viewport_width => "portrait",
            Current::Orientation => "landscape",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    Lt,
    Le,
    Eq,
    Ge,
    Gt,
}

impl Comparison {
    /// `value OP feature` → `feature OP' value`.
    fn flip(self) -> Self {
        match self {
            Self::Lt => Self::Gt,
            Self::Le => Self::Ge,
            Self::Eq => Self::Eq,
            Self::Ge => Self::Le,
            Self::Gt => Self::Lt,
        }
    }

    fn holds(self, actual: f32, value: f32) -> bool {
        match self {
            Self::Lt => actual < value,
            Self::Le => actual <= value,
            Self::Eq => actual == value,
            Self::Ge => actual >= value,
            Self::Gt => actual > value,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LengthUnit {
    Px,
    Em,
    Rem,
    Vw,
    Vh,
    Vmin,
    Vmax,
    Cm,
    Mm,
    Q,
    In,
    Pt,
    Pc,
}

impl LengthUnit {
    fn from_name(unit: &str) -> Option<Self> {
        Some(match unit.to_ascii_lowercase().as_str() {
            "px" => Self::Px,
            "em" => Self::Em,
            "rem" => Self::Rem,
            "vw" => Self::Vw,
            "vh" => Self::Vh,
            "vmin" => Self::Vmin,
            "vmax" => Self::Vmax,
            "cm" => Self::Cm,
            "mm" => Self::Mm,
            "q" => Self::Q,
            "in" => Self::In,
            "pt" => Self::Pt,
            "pc" => Self::Pc,
            _ => return None,
        })
    }

    fn to_px(self, value: f32, ctx: &MediaContext) -> f32 {
        let (w, h) = (ctx.viewport_width, ctx.viewport_height);
        value
            * match self {
                Self::Px => 1.0,
                Self::Em | Self::Rem => INITIAL_FONT_SIZE,
                Self::Vw => w / 100.0,
                Self::Vh => h / 100.0,
                Self::Vmin => w.min(h) / 100.0,
                Self::Vmax => w.max(h) / 100.0,
                Self::Cm => 96.0 / 2.54,
                Self::Mm => 96.0 / 25.4,
                Self::Q => 96.0 / 101.6,
                Self::In => 96.0,
                Self::Pt => 96.0 / 72.0,
                Self::Pc => 16.0,
            }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MediaValue {
    /// A length, or the sum of a constant `calc()` (`48rem - .02px` →
    /// `[(48, rem), (-0.02, px)]`). Only `+`/`-` of plain lengths until
    /// compat round 3 step 5 brings real `calc`.
    Length(Vec<(f32, LengthUnit)>),
    Ratio(f32, f32),
    /// In dppx.
    Resolution(f32),
    Number(f32),
}

impl MediaValue {
    fn resolve(&self, ctx: &MediaContext) -> f32 {
        match self {
            Self::Length(terms) => terms.iter().map(|(v, unit)| unit.to_px(*v, ctx)).sum(),
            Self::Ratio(a, b) => ratio(*a, *b),
            Self::Resolution(dppx) => *dppx,
            Self::Number(n) => *n,
        }
    }
}

/// `a / b`; a degenerate `0/0` is NaN, which no comparison holds for.
fn ratio(a: f32, b: f32) -> f32 {
    if b == 0.0 {
        if a == 0.0 { f32::NAN } else { f32::INFINITY }
    } else {
        a / b
    }
}

// =============================================================================
// Evaluation
// =============================================================================

/// Whether `list` matches. The function `@media` blocks, `<link media>` /
/// `<style media>` (as blocks) and — later — `matchMedia` all go through.
pub fn evaluate(list: &MediaQueryList, ctx: &MediaContext) -> bool {
    list.queries.is_empty() || list.queries.iter().any(|query| query.matches(ctx))
}

impl MediaQuery {
    pub fn matches(&self, ctx: &MediaContext) -> bool {
        let MediaQuery::Query {
            negated,
            media_type,
            condition,
        } = self
        else {
            return false;
        };
        let type_matches = matches!(media_type, MediaType::All | MediaType::Screen);
        let condition = match condition {
            Some(condition) => condition.evaluate(ctx),
            None => Some(true),
        };
        match condition {
            // Unknown is false, negated or not (MQ4).
            None => false,
            Some(condition) => (type_matches && condition) != *negated,
        }
    }
}

impl MediaCondition {
    /// Kleene logic: `None` = unknown.
    fn evaluate(&self, ctx: &MediaContext) -> Option<bool> {
        match self {
            Self::Feature(feature) => feature.evaluate(ctx),
            Self::Not(inner) => inner.evaluate(ctx).map(|value| !value),
            Self::And(items) => {
                let mut result = Some(true);
                for item in items {
                    match item.evaluate(ctx) {
                        Some(false) => return Some(false),
                        None => result = None,
                        Some(true) => {}
                    }
                }
                result
            }
            Self::Or(items) => {
                let mut result = Some(false);
                for item in items {
                    match item.evaluate(ctx) {
                        Some(true) => return Some(true),
                        None => result = None,
                        Some(false) => {}
                    }
                }
                result
            }
            Self::Unknown => None,
        }
    }
}

impl MediaFeature {
    fn evaluate(&self, ctx: &MediaContext) -> Option<bool> {
        Some(match self {
            Self::Range(feature, comparisons) => {
                let actual = feature.actual(ctx);
                comparisons
                    .iter()
                    .all(|(op, value)| op.holds(actual, value.resolve(ctx)))
            }
            Self::RangeBoolean(feature) => feature.actual(ctx) != 0.0,
            Self::Discrete(feature, Some(value)) => feature.current(ctx) == *value,
            Self::Discrete(feature, None) => Some(feature.current(ctx)) != feature.false_value,
        })
    }
}

// =============================================================================
// `@media` blocks in a stylesheet
// =============================================================================

/// One `@media` block of a parsed sheet; every rule inside points at it
/// (`Rule::media`). A nested block's `parent` is the enclosing one — the
/// rule applies when the whole chain matches (nested `@media` = AND).
#[derive(Debug, Clone, PartialEq)]
pub struct MediaBlock {
    pub queries: MediaQueryList,
    pub parent: Option<Rc<MediaBlock>>,
    /// Position in `Stylesheet::media` (parents come before children).
    pub index: usize,
}

/// Evaluate every `@media` block of a sheet once: `result[block.index]`.
/// The style pass calls this at its start, never per node.
pub fn evaluate_blocks(blocks: &[Rc<MediaBlock>], ctx: &MediaContext) -> Vec<bool> {
    let mut results = Vec::with_capacity(blocks.len());
    for block in blocks {
        let parent = block
            .parent
            .as_ref()
            .is_none_or(|parent| results.get(parent.index).copied().unwrap_or(false));
        results.push(parent && evaluate(&block.queries, ctx));
    }
    results
}

/// Every block result of every sheet packed into bits — the view cache key
/// (`BrowserState`): a resize that crosses no query boundary keeps it.
pub fn media_bitset<'a>(
    sheets: impl IntoIterator<Item = &'a [Rc<MediaBlock>]>,
    ctx: &MediaContext,
) -> Vec<u64> {
    let mut bits = Vec::new();
    let mut at = 0usize;
    for blocks in sheets {
        for matched in evaluate_blocks(blocks, ctx) {
            if at.is_multiple_of(64) {
                bits.push(0);
            }
            if matched && let Some(word) = bits.last_mut() {
                *word |= 1 << (at % 64);
            }
            at += 1;
        }
        // A sheet boundary also marks where the next sheet's bits start,
        // so two sheets' blocks can't trade places unnoticed.
        bits.push(at as u64);
    }
    bits
}

// =============================================================================
// Parsing
// =============================================================================

type Fail = ParseError<()>;

fn fail<T>() -> Result<T, Fail> {
    Err(ParseError::custom(()))
}

impl MediaQueryList {
    /// Parse a `media` attribute or a standalone prelude.
    pub fn parse(source: &str) -> Self {
        let mut parser = Parser::new(source);
        Self::parse_from(&mut parser)
    }

    /// Parse the rest of `input` (an `@media` prelude) as a query list.
    /// Never fails: an item that doesn't parse becomes [`MediaQuery::Invalid`].
    pub fn parse_from(input: &mut Parser<'_>) -> Self {
        let mut queries = Vec::new();
        if input.is_exhausted() {
            return Self { queries };
        }
        loop {
            let query = input
                .parse_until_before(Delimiter::Comma, |input| {
                    let query = parse_query(input)?;
                    input.expect_exhausted()?;
                    Ok::<_, Fail>(query)
                })
                .unwrap_or(MediaQuery::Invalid);
            queries.push(query);
            // `parse_until_before` stops at the comma (or the end) whether
            // the item parsed or not.
            match input.next() {
                Ok(Token::Comma) => continue,
                _ => break,
            }
        }
        Self { queries }
    }

    /// Comma items that failed to parse (survey count).
    pub fn invalid_queries(&self) -> usize {
        self.queries
            .iter()
            .filter(|query| matches!(query, MediaQuery::Invalid))
            .count()
    }
}

fn parse_query(input: &mut Parser<'_>) -> Result<MediaQuery, Fail> {
    if let Ok(query) = input.try_parse(parse_typed_query) {
        return Ok(query);
    }
    let condition = parse_condition(input, true, 0)?;
    Ok(MediaQuery::Query {
        negated: false,
        media_type: MediaType::All,
        condition: Some(condition),
    })
}

/// `[not | only]? <media-type> [and <media-condition-without-or>]?`
fn parse_typed_query(input: &mut Parser<'_>) -> Result<MediaQuery, Fail> {
    let first = input.expect_ident_cloned()?.to_ascii_lowercase();
    let (negated, name) = match first.as_str() {
        "not" => (true, input.expect_ident_cloned()?.to_ascii_lowercase()),
        "only" => (false, input.expect_ident_cloned()?.to_ascii_lowercase()),
        _ => (false, first),
    };
    let media_type = match name.as_str() {
        "not" | "only" | "and" | "or" | "layer" => return fail(),
        "all" => MediaType::All,
        "screen" => MediaType::Screen,
        _ => MediaType::Other,
    };
    let condition = if input
        .try_parse(|input| input.expect_ident_matching("and"))
        .is_ok()
    {
        Some(parse_condition(input, false, 0)?)
    } else {
        None
    };
    input.expect_exhausted()?;
    Ok(MediaQuery::Query {
        negated,
        media_type,
        condition,
    })
}

/// `not <in-parens>` | `<in-parens> [and <in-parens>]*` | `… [or …]*`.
fn parse_condition(
    input: &mut Parser<'_>,
    allow_or: bool,
    depth: usize,
) -> Result<MediaCondition, Fail> {
    if input
        .try_parse(|input| input.expect_ident_matching("not"))
        .is_ok()
    {
        let inner = parse_in_parens(input, depth)?;
        return Ok(MediaCondition::Not(Box::new(inner)));
    }
    let mut items = vec![parse_in_parens(input, depth)?];
    let mut is_or = None;
    loop {
        let word = input.try_parse(|input| {
            let word = input.expect_ident_cloned()?.to_ascii_lowercase();
            match (word.as_str(), is_or) {
                ("and", None | Some(false)) => Ok(false),
                ("or", None | Some(true)) if allow_or => Ok(true),
                _ => fail(),
            }
        });
        let Ok(or) = word else { break };
        is_or = Some(or);
        items.push(parse_in_parens(input, depth)?);
    }
    Ok(match (is_or, items.len()) {
        (_, 1) => items.pop().unwrap_or(MediaCondition::Unknown),
        (Some(true), _) => MediaCondition::Or(items),
        _ => MediaCondition::And(items),
    })
}

/// `( <condition> )` | `( <feature> )` | `<general-enclosed>`.
fn parse_in_parens(input: &mut Parser<'_>, depth: usize) -> Result<MediaCondition, Fail> {
    match input.next()?.clone() {
        Token::ParenthesisBlock => input.parse_nested_block(|input| {
            if depth + 1 >= MAX_MEDIA_NESTING {
                return Ok(general_enclosed(input));
            }
            if let Ok(condition) = input.try_parse(|input| {
                let condition = parse_condition(input, true, depth + 1)?;
                input.expect_exhausted()?;
                Ok::<_, Fail>(condition)
            }) {
                return Ok(condition);
            }
            if let Ok(condition) = input.try_parse(parse_feature) {
                return Ok(condition);
            }
            Ok(general_enclosed(input))
        }),
        // `<function-token> <any-value>? )`
        Token::Function(_) => input.parse_nested_block(|input| Ok(general_enclosed(input))),
        _ => fail(),
    }
}

fn general_enclosed(input: &mut Parser<'_>) -> MediaCondition {
    while input.next().is_ok() {}
    MediaCondition::Unknown
}

/// The inside of `( … )` as a feature. A well-formed feature we don't know
/// (or with a value it doesn't take) is `Unknown`, not an error.
fn parse_feature(input: &mut Parser<'_>) -> Result<MediaCondition, Fail> {
    if let Ok(condition) = input.try_parse(parse_plain_or_boolean) {
        return Ok(condition);
    }
    parse_range(input)
}

/// `name` | `name: value`.
fn parse_plain_or_boolean(input: &mut Parser<'_>) -> Result<MediaCondition, Fail> {
    let name = input.expect_ident_cloned()?.to_ascii_lowercase();
    if input.is_exhausted() {
        return Ok(boolean_feature(&name));
    }
    input.expect_colon()?;
    let value = match input.try_parse(|input| input.expect_ident_cloned()) {
        Ok(ident) => RawValue::Ident(ident.to_ascii_lowercase()),
        Err(_) => parse_raw_value(input)?,
    };
    input.expect_exhausted()?;
    Ok(plain_feature(&name, value))
}

fn boolean_feature(name: &str) -> MediaCondition {
    if let Some(feature) = RangeFeature::from_name(name) {
        MediaCondition::Feature(MediaFeature::RangeBoolean(feature))
    } else if let Some(feature) = DiscreteFeature::lookup(name) {
        MediaCondition::Feature(MediaFeature::Discrete(feature, None))
    } else {
        MediaCondition::Unknown
    }
}

fn plain_feature(name: &str, value: RawValue) -> MediaCondition {
    let (comparison, base) = match name {
        "-webkit-min-device-pixel-ratio" => (Comparison::Ge, Some(RangeFeature::DevicePixelRatio)),
        "-webkit-max-device-pixel-ratio" => (Comparison::Le, Some(RangeFeature::DevicePixelRatio)),
        "-webkit-device-pixel-ratio" => (Comparison::Eq, Some(RangeFeature::DevicePixelRatio)),
        _ => match (name.strip_prefix("min-"), name.strip_prefix("max-")) {
            (Some(rest), _) => (Comparison::Ge, RangeFeature::from_name(rest)),
            (_, Some(rest)) => (Comparison::Le, RangeFeature::from_name(rest)),
            _ => (Comparison::Eq, RangeFeature::from_name(name)),
        },
    };
    if let Some(feature) = base {
        return match value.coerce(feature.kind()) {
            Some(value) => {
                MediaCondition::Feature(MediaFeature::Range(feature, vec![(comparison, value)]))
            }
            None => MediaCondition::Unknown,
        };
    }
    match (DiscreteFeature::lookup(name), value) {
        (Some(feature), RawValue::Ident(ident)) => {
            match feature.values.iter().find(|v| **v == ident) {
                Some(value) => {
                    MediaCondition::Feature(MediaFeature::Discrete(feature, Some(*value)))
                }
                None => MediaCondition::Unknown,
            }
        }
        _ => MediaCondition::Unknown,
    }
}

/// `name OP value` | `value OP name` | `value OP name OP value`.
fn parse_range(input: &mut Parser<'_>) -> Result<MediaCondition, Fail> {
    if let Ok(name) = input.try_parse(|input| input.expect_ident_cloned()) {
        let name = name.to_ascii_lowercase();
        let op = parse_comparison(input)?;
        let value = parse_raw_value(input)?;
        input.expect_exhausted()?;
        return Ok(range_feature(&name, vec![(op, value)]));
    }
    let first = parse_raw_value(input)?;
    let first_op = parse_comparison(input)?.flip();
    let name = input.expect_ident_cloned()?.to_ascii_lowercase();
    if input.is_exhausted() {
        return Ok(range_feature(&name, vec![(first_op, first)]));
    }
    let second_op = parse_comparison(input)?;
    let second = parse_raw_value(input)?;
    input.expect_exhausted()?;
    // `a < x < b` / `a > x > b` only: both the same direction, no `=`.
    // (`first_op` is flipped, so `a < x` reads `x > a`.)
    let same_direction = matches!(
        (first_op, second_op),
        (
            Comparison::Gt | Comparison::Ge,
            Comparison::Lt | Comparison::Le
        ) | (
            Comparison::Lt | Comparison::Le,
            Comparison::Gt | Comparison::Ge
        )
    );
    if !same_direction {
        return fail();
    }
    Ok(range_feature(
        &name,
        vec![(first_op, first), (second_op, second)],
    ))
}

fn range_feature(name: &str, comparisons: Vec<(Comparison, RawValue)>) -> MediaCondition {
    // `min-`/`max-` prefixes and discrete features don't take range syntax.
    let Some(feature) = RangeFeature::from_name(name) else {
        return MediaCondition::Unknown;
    };
    let mut resolved = Vec::with_capacity(comparisons.len());
    for (op, value) in comparisons {
        match value.coerce(feature.kind()) {
            Some(value) => resolved.push((op, value)),
            None => return MediaCondition::Unknown,
        }
    }
    MediaCondition::Feature(MediaFeature::Range(feature, resolved))
}

/// `<` `<=` `>` `>=` `=` (no space inside `<=`).
fn parse_comparison(input: &mut Parser<'_>) -> Result<Comparison, Fail> {
    let first = match input.next()? {
        Token::Delim('<') => Comparison::Lt,
        Token::Delim('>') => Comparison::Gt,
        Token::Delim('=') => return Ok(Comparison::Eq),
        _ => return fail(),
    };
    let or_equal = input
        .try_parse(|input| match input.next_including_whitespace() {
            Ok(Token::Delim('=')) => Ok(()),
            _ => fail(),
        })
        .is_ok();
    Ok(match (first, or_equal) {
        (Comparison::Lt, true) => Comparison::Le,
        (Comparison::Gt, true) => Comparison::Ge,
        (op, _) => op,
    })
}

/// A value before we know which feature it is for (range syntax may put
/// the value first).
enum RawValue {
    Ident(String),
    Length(Vec<(f32, LengthUnit)>),
    Number { value: f32, integer: bool },
    Ratio(f32, f32),
    Resolution(f32),
}

impl RawValue {
    fn coerce(self, kind: ValueKind) -> Option<MediaValue> {
        match (kind, self) {
            (ValueKind::Length, RawValue::Length(terms)) => Some(MediaValue::Length(terms)),
            (ValueKind::Length, RawValue::Number { value: 0.0, .. }) => {
                Some(MediaValue::Length(vec![(0.0, LengthUnit::Px)]))
            }
            (ValueKind::Ratio, RawValue::Ratio(a, b)) if a >= 0.0 && b >= 0.0 => {
                Some(MediaValue::Ratio(a, b))
            }
            (ValueKind::Ratio, RawValue::Number { value, .. }) if value >= 0.0 => {
                Some(MediaValue::Ratio(value, 1.0))
            }
            (ValueKind::Resolution, RawValue::Resolution(dppx)) => {
                Some(MediaValue::Resolution(dppx))
            }
            (ValueKind::Integer, RawValue::Number { value, integer })
                if integer && value >= 0.0 =>
            {
                Some(MediaValue::Number(value))
            }
            (ValueKind::Number, RawValue::Number { value, .. }) => Some(MediaValue::Number(value)),
            _ => None,
        }
    }
}

fn parse_raw_value(input: &mut Parser<'_>) -> Result<RawValue, Fail> {
    match input.next()?.clone() {
        Token::Dimension { value, unit, .. } => {
            if let Some(unit) = LengthUnit::from_name(&unit) {
                return Ok(RawValue::Length(vec![(value, unit)]));
            }
            let dppx = match unit.to_ascii_lowercase().as_str() {
                "dppx" | "x" => value,
                "dpi" => value / 96.0,
                "dpcm" => value * 2.54 / 96.0,
                _ => return fail(),
            };
            Ok(RawValue::Resolution(dppx))
        }
        Token::Number {
            value, int_value, ..
        } => {
            let denominator = input.try_parse(|input| {
                input.expect_delim('/')?;
                Ok::<_, Fail>(input.expect_number()?)
            });
            Ok(match denominator {
                Ok(b) => RawValue::Ratio(value, b),
                Err(_) => RawValue::Number {
                    value,
                    integer: int_value.is_some(),
                },
            })
        }
        Token::Function(name)
            if name.eq_ignore_ascii_case("calc") || name.eq_ignore_ascii_case("-webkit-calc") =>
        {
            input.parse_nested_block(parse_constant_calc)
        }
        _ => fail(),
    }
}

/// The narrow `calc()` of 2.1: lengths joined by ` + ` / ` - `.
fn parse_constant_calc(input: &mut Parser<'_>) -> Result<RawValue, Fail> {
    let mut terms = vec![parse_calc_length(input)?];
    while !input.is_exhausted() {
        let sign = match input.next()? {
            Token::Delim('+') => 1.0,
            Token::Delim('-') => -1.0,
            _ => return fail(),
        };
        let (value, unit) = parse_calc_length(input)?;
        terms.push((sign * value, unit));
    }
    Ok(RawValue::Length(terms))
}

fn parse_calc_length(input: &mut Parser<'_>) -> Result<(f32, LengthUnit), Fail> {
    match input.next()? {
        Token::Dimension { value, unit, .. } => match LengthUnit::from_name(unit) {
            Some(unit) => Ok((*value, unit)),
            None => fail(),
        },
        _ => fail(),
    }
}

// =============================================================================
// `<link media>` / `<style media>`
// =============================================================================

/// A sheet's text as it joins the document's other sheets: wrapped in
/// `@media {media} { … }` when the element's `media` attribute limits it
/// (docs/compat-round3.md 2.1 — the sheets are joined into one string, so
/// the attribute has to travel inside the CSS).
///
/// Two things are rewritten so the wrapper can't be broken from inside:
/// comma items of the attribute holding `{ } ; " ' \` or `/*` (which would
/// end or swallow the prelude) are dropped — each would be an invalid query
/// anyway, i.e. `not all`; and a stray top-level `}` in the sheet (which
/// would close the wrapper early and let the rest apply everywhere) becomes
/// `\}` — an ident, which drops the same rule the stray `}` dropped.
pub fn scope_sheet<'a>(css: &'a str, media: Option<&str>) -> Cow<'a, str> {
    let Some(media) = media.map(str::trim) else {
        return Cow::Borrowed(css);
    };
    if media.is_empty() || media.eq_ignore_ascii_case("all") {
        return Cow::Borrowed(css);
    }
    let safe: Vec<&str> = media
        .split(',')
        .map(str::trim)
        .filter(|item| !item.contains(['{', '}', ';', '"', '\'', '\\']) && !item.contains("/*"))
        .collect();
    let prelude = if safe.is_empty() || safe.iter().all(|item| item.is_empty()) {
        "not all".to_string()
    } else {
        safe.join(", ")
    };
    let mut out = String::with_capacity(css.len() + prelude.len() + 16);
    out.push_str("@media ");
    out.push_str(&prelude);
    out.push_str(" {\n");
    push_without_stray_closers(css, &mut out);
    out.push_str("\n}\n");
    Cow::Owned(out)
}

fn push_without_stray_closers(css: &str, out: &mut String) {
    let mut parser = Parser::new(css);
    let mut copied = 0;
    loop {
        let before = parser.position();
        let token = match parser.next_including_whitespace_and_comments() {
            Ok(token) => token.clone(),
            Err(_) => break,
        };
        if token == Token::CloseCurlyBracket {
            let at = before.byte_index();
            out.push_str(css.get(copied..at).unwrap_or_default());
            out.push_str("\\}");
            copied = at + 1;
        }
    }
    out.push_str(css.get(copied..).unwrap_or_default());
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESKTOP: MediaContext = MediaContext::new(1280.0, 800.0);

    fn matches(query: &str) -> bool {
        evaluate(&MediaQueryList::parse(query), &DESKTOP)
    }

    #[test]
    fn media_width_queries() {
        assert!(matches("(min-width:768px)"));
        assert!(!matches("(max-width:767px)"));
        assert!(matches("(width>=768px)"));
        assert!(!matches("(400px<=width<=700px)"));
        assert!(!matches("(width<calc(48rem - .02px))"));
        assert!(matches("(width >= calc(48rem - .02px))"));
        assert!(matches("(1000px < width < 1300px)"));
        assert!(matches("(1300px > width > 1000px)"));
        assert!(!matches("(1300px > width < 1000px)"), "mixed directions");
        assert!(matches("(min-height: 800px) and (max-height: 800px)"));
        assert!(matches("(height = 800px)"));
        assert!(matches("(width > 50vw)"));
        assert!(matches("(min-width: 0)"));
        assert!(matches("(orientation: landscape)"));
        assert!(!matches("(orientation: portrait)"));
        assert!(matches("(min-aspect-ratio: 16/10)"));
        assert!(!matches("(min-aspect-ratio: 16/9)"));
        assert!(matches("(aspect-ratio > 1)"));
    }

    #[test]
    fn media_types() {
        assert!(!matches("print"));
        assert!(matches("screen and (min-width:1px)"));
        assert!(matches("not print"));
        assert!(matches("only screen"));
        assert!(matches("all"));
        assert!(!matches("not screen"));
        assert!(!matches("speech"));
        assert!(!matches("tv"));
        assert!(matches("SCREEN AND (MIN-WIDTH: 1PX)"));
        assert!(!matches("not all and (min-width: 1px)"));
        assert!(matches("not all and (max-width: 1px)"));
    }

    #[test]
    fn media_preferences() {
        assert!(!matches("(prefers-color-scheme:dark)"));
        assert!(matches("(prefers-color-scheme: light)"));
        assert!(!matches("(prefers-reduced-motion:reduce)"));
        assert!(matches("(prefers-reduced-motion: no-preference)"));
        assert!(!matches("(prefers-reduced-motion)"));
        assert!(!matches("(forced-colors:active)"));
        assert!(matches("(forced-colors: none)"));
        assert!(matches("(hover:hover)"));
        assert!(matches("(hover)"));
        assert!(matches("(pointer: fine)"));
        assert!(!matches("(pointer: coarse)"));
        assert!(matches("(any-hover: hover) and (any-pointer: fine)"));
        assert!(!matches("(-ms-high-contrast: active)"));
        assert!(!matches("(-ms-high-contrast: none)"));
        assert!(matches("(resolution: 1dppx)"));
        assert!(!matches("(min-resolution: 2dppx)"));
        assert!(!matches("(-webkit-min-device-pixel-ratio: 2)"));
        assert!(matches("(-webkit-min-device-pixel-ratio: 1)"));
        assert!(matches("(max-resolution: 96dpi)"));
    }

    #[test]
    fn media_unknown_feature_is_false() {
        assert!(!matches("(frobnicate: 1)"));
        assert!(!matches("(min-widht: 1px)"));
        assert!(
            !matches("(min-width: wide)"),
            "a known feature, a value it doesn't take"
        );
        assert!(
            !matches("(--custom-media)"),
            "@custom-media is out of scope"
        );
        assert!(!matches("screen and (frobnicate)"));
        assert!(!matches("(frobnicate) or (frobnicate)"));
        assert!(
            matches("(frobnicate) or (min-width: 1px)"),
            "unknown or true = true"
        );
        assert!(!matches("(frobnicate) and (min-width: 1px)"));
        assert!(!matches("(width < calc(10px * 2))"), "only + and - in calc");
    }

    #[test]
    fn media_not_unknown_is_false() {
        assert!(!matches("not (frobnicate: 1)"));
        assert!(!matches("not all and (frobnicate)"));
        assert!(!matches("not (prefers-reduced-data: reduce)"));
        assert!(!matches("not (-ms-high-contrast: active)"));
        assert!(matches("not (prefers-color-scheme: dark)"));
        assert!(!matches("not (not (frobnicate))"));
    }

    #[test]
    fn media_em_uses_16px() {
        assert!(matches("(min-width: 80em)"));
        assert!(!matches("(min-width: 80.1em)"));
        assert!(matches("(max-width: 80rem)"));
        let small = MediaContext::new(500.0, 800.0);
        assert!(evaluate(
            &MediaQueryList::parse("(max-width: 40em)"),
            &small
        ));
    }

    #[test]
    fn media_desktop_defaults_true() {
        for query in [
            "(scripting: enabled)",
            "(display-mode: browser)",
            "(prefers-contrast: no-preference)",
            "(color)",
            "(min-color: 8)",
            "(color-gamut: srgb)",
            "(dynamic-range: standard)",
            "(inverted-colors: none)",
            "(update: fast)",
            "(prefers-reduced-transparency: no-preference)",
        ] {
            assert!(matches(query), "{query}");
        }
        for query in [
            "(scripting: none)",
            "(display-mode: standalone)",
            "(color-gamut: p3)",
            "(dynamic-range: high)",
            "(monochrome)",
            "(update: slow)",
        ] {
            assert!(!matches(query), "{query}");
        }
    }

    #[test]
    fn media_list_invalid_item_only_drops_that_item() {
        let list = MediaQueryList::parse("screen and, (min-width: 1px)");
        assert_eq!(list.invalid_queries(), 1);
        assert!(evaluate(&list, &DESKTOP));
        let list = MediaQueryList::parse("(min-width: 1px) and or (color), print");
        assert_eq!(list.invalid_queries(), 1);
        assert!(!evaluate(&list, &DESKTOP));
        assert!(
            !matches("screen and (a) or (b)"),
            "no `or` after a media type"
        );
        assert!(!matches("only (color)"));
        assert!(!matches(","), "two empty items, both invalid");
        assert!(matches(""), "an empty list is `all`");
        assert!(!matches("(min-width: 1px) junk"));
    }

    #[test]
    fn deep_parentheses_are_bounded() {
        let deep = format!(
            "{}(min-width: 1px){}",
            "(".repeat(10_000),
            ")".repeat(10_000)
        );
        assert!(!matches(&deep), "past the nesting cap reads as unknown");
        let shallow = format!("{}(min-width: 1px){}", "(".repeat(5), ")".repeat(5));
        assert!(matches(&shallow));
        let unbalanced = "(".repeat(100_000);
        assert!(!matches(&unbalanced));
    }

    #[test]
    fn huge_query_lists_stay_linear() {
        let many = vec!["(max-width: 1px)"; 50_000].join(", ") + ", screen";
        let list = MediaQueryList::parse(&many);
        assert_eq!(list.queries.len(), 50_001);
        assert!(evaluate(&list, &DESKTOP));
    }

    #[test]
    fn nested_blocks_are_anded() {
        let outer = Rc::new(MediaBlock {
            queries: MediaQueryList::parse("screen"),
            parent: None,
            index: 0,
        });
        let inner = Rc::new(MediaBlock {
            queries: MediaQueryList::parse("(max-width: 600px)"),
            parent: Some(Rc::clone(&outer)),
            index: 1,
        });
        let innermost = Rc::new(MediaBlock {
            queries: MediaQueryList::parse("(color)"),
            parent: Some(Rc::clone(&inner)),
            index: 2,
        });
        let blocks = [outer, inner, innermost];
        assert_eq!(evaluate_blocks(&blocks, &DESKTOP), [true, false, false]);
        let narrow = MediaContext::new(500.0, 800.0);
        assert_eq!(evaluate_blocks(&blocks, &narrow), [true, true, true]);
    }

    #[test]
    fn bitset_changes_only_when_a_result_does() {
        let blocks: Vec<_> = ["(max-width: 600px)", "(min-height: 700px)"]
            .iter()
            .enumerate()
            .map(|(index, query)| {
                Rc::new(MediaBlock {
                    queries: MediaQueryList::parse(query),
                    parent: None,
                    index,
                })
            })
            .collect();
        let key = |w, h| media_bitset([blocks.as_slice()], &MediaContext::new(w, h));
        assert_eq!(key(1280.0, 800.0), key(1000.0, 750.0));
        assert_ne!(key(1280.0, 800.0), key(1280.0, 600.0));
        assert_ne!(key(1280.0, 800.0), key(500.0, 800.0));
    }

    #[test]
    fn scope_sheet_wraps_only_limiting_media() {
        assert_eq!(scope_sheet("a{}", None), "a{}");
        assert_eq!(scope_sheet("a{}", Some(" ")), "a{}");
        assert_eq!(scope_sheet("a{}", Some("ALL")), "a{}");
        assert_eq!(
            scope_sheet("a{}", Some("print")),
            "@media print {\na{}\n}\n"
        );
        assert_eq!(
            scope_sheet("a{}", Some("print, x{y}")),
            "@media print {\na{}\n}\n"
        );
        assert_eq!(
            scope_sheet("a{}", Some("x{y}")),
            "@media not all {\na{}\n}\n"
        );
    }

    #[test]
    fn scope_sheet_neutralises_a_stray_closing_brace() {
        let scoped = scope_sheet("a{color:red} } b{color:blue} s{content:'}'}", Some("print"));
        assert_eq!(
            scoped,
            "@media print {\na{color:red} \\} b{color:blue} s{content:'}'}\n}\n"
        );
        let sheet = crate::css::parse(&scoped).unwrap();
        assert!(
            sheet.rules.iter().all(|rule| rule.media.is_some()),
            "nothing escapes the wrapper: {sheet:?}"
        );
    }
}
