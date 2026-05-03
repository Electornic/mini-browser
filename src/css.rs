// CSS support is intentionally narrow: simple selectors and a handful of value types.
// That keeps the parser small while still giving the rest of the browser realistic input.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub selectors: Vec<Selector>,
    pub declarations: Vec<Declaration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimpleSelectorKind {
    Tag(String),
    Class(String),
    Id(String),
}

/// Pseudo-classes attached to a simple selector, e.g. the `:hover` in `.btn:hover`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PseudoClass {
    Hover,
    Focus,
    Active,
    /// `:link` — unvisited anchor. Without browsing-history tracking we
    /// treat every `<a href>` as unvisited, so `:link` always matches a
    /// real anchor and `:visited` never does. That choice keeps author
    /// rules like HN's `a:link { color: black }` / `a:visited { ... }`
    /// behaving the way fresh visitors see the page.
    Link,
    /// `:visited` — visited anchor. See `Link` above; this never matches.
    Visited,
}

/// A single simple selector position: a tag/class/id base plus an optional
/// pseudo-class. `.btn:hover` parses to one SimpleSelector with
/// `kind = Class("btn")` and `pseudo = Some(Hover)`. Standalone pseudo-classes
/// (a bare `:hover`) are not supported yet — every simple selector still needs
/// a tag/class/id base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimpleSelector {
    pub kind: SimpleSelectorKind,
    pub pseudo: Option<PseudoClass>,
}

impl SimpleSelector {
    pub fn tag(name: impl Into<String>) -> Self {
        Self {
            kind: SimpleSelectorKind::Tag(name.into()),
            pseudo: None,
        }
    }

    pub fn class(name: impl Into<String>) -> Self {
        Self {
            kind: SimpleSelectorKind::Class(name.into()),
            pseudo: None,
        }
    }

    pub fn id(name: impl Into<String>) -> Self {
        Self {
            kind: SimpleSelectorKind::Id(name.into()),
            pseudo: None,
        }
    }

    pub fn with_pseudo(mut self, pseudo: PseudoClass) -> Self {
        self.pseudo = Some(pseudo);
        self
    }
}

/// How two adjacent simple selectors in a complex selector are related.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combinator {
    /// Whitespace combinator: the right side is some descendant of the left side.
    Descendant,
    /// `>` combinator: the right side must be the immediate child of the left side.
    Child,
}

/// A complex selector is a list of simple selectors joined by combinators.
/// `parts` is ordered left-to-right (outermost ancestor first, target element last).
/// `combinators` is parallel to the boundaries between consecutive parts, so its length
/// is always `parts.len().saturating_sub(1)`. A length-1 selector is a plain simple
/// selector with no combinators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selector {
    pub parts: Vec<SimpleSelector>,
    pub combinators: Vec<Combinator>,
}

impl Selector {
    pub fn simple(part: SimpleSelector) -> Self {
        Self {
            parts: vec![part],
            combinators: Vec::new(),
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
    let mut parser = Parser::new(source);
    let stylesheet = parser.parse_stylesheet_tolerant();
    Ok(stylesheet)
}

/// Parse a single complex selector (e.g. `div.card > a`). Used by JS-facing
/// `document.querySelector` to reuse the existing selector grammar without
/// going through a full stylesheet rule. Trailing input after the selector is
/// rejected so callers can't smuggle declarations through the same entry point.
pub fn parse_selector(input: &str) -> Result<Selector, ParseError> {
    let mut parser = Parser::new(input);
    parser.skip_whitespace_and_comments();
    let selector = parser.parse_selector()?;
    parser.skip_whitespace_and_comments();
    if !parser.eof() {
        return Err(ParseError::new(
            parser.pos,
            "unexpected trailing input after selector",
        ));
    }
    Ok(selector)
}

/// Flips the sign of a numeric value while leaving non-numeric values untouched.
/// Used by the value parser to apply a leading minus to lengths and unitless
/// numbers (e.g. `-10px`, `z-index: -2`).
fn negate_numeric(value: Value) -> Value {
    match value {
        Value::Length(v, unit) => Value::Length(-v, unit),
        Value::Number(v) => Value::Number(-v),
        other => other,
    }
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

struct Parser<'a> {
    pos: usize,
    input: &'a str,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self { pos: 0, input }
    }

    fn parse_stylesheet_tolerant(&mut self) -> Stylesheet {
        let mut rules = Vec::new();

        loop {
            self.skip_whitespace_and_comments();

            if self.eof() {
                break;
            }

            // Skip at-rules (@media, @charset, @keyframes, etc.) the toy parser does not model.
            if self.next_char() == Some('@') {
                self.skip_at_rule();
                continue;
            }

            // Try parsing a normal rule; skip to the next block boundary on failure.
            let saved = self.pos;
            match self.parse_rule() {
                Ok(rule) => rules.push(rule),
                Err(_) => {
                    self.pos = saved;
                    self.skip_to_end_of_block();
                }
            }
        }

        Stylesheet { rules }
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            self.consume_whitespace();
            if self.starts_with("/*") {
                self.pos += 2;
                while !self.eof() && !self.starts_with("*/") {
                    if let Some(ch) = self.next_char() {
                        self.pos += ch.len_utf8();
                    }
                }
                if self.starts_with("*/") {
                    self.pos += 2;
                }
            } else {
                break;
            }
        }
    }

    fn skip_at_rule(&mut self) {
        // At-rules either end with ';' or contain a '{...}' block.
        let mut brace_depth = 0;
        while let Some(ch) = self.next_char() {
            self.pos += ch.len_utf8();
            match ch {
                '{' => brace_depth += 1,
                '}' if brace_depth > 1 => brace_depth -= 1,
                '}' if brace_depth == 1 => return,
                ';' if brace_depth == 0 => return,
                _ => {}
            }
        }
    }

    fn skip_to_end_of_block(&mut self) {
        // Advance past the next '}' so the parser can attempt the next rule.
        let mut brace_depth = 0;
        while let Some(ch) = self.next_char() {
            self.pos += ch.len_utf8();
            match ch {
                '{' => brace_depth += 1,
                '}' if brace_depth > 1 => brace_depth -= 1,
                '}' => return,
                _ => {}
            }
        }
    }

    fn starts_with(&self, value: &str) -> bool {
        self.input[self.pos..].starts_with(value)
    }

    fn parse_rule(&mut self) -> Result<Rule, ParseError> {
        let selectors = self.parse_selectors()?;
        self.consume_whitespace();
        self.expect_char('{')?;
        let declarations = self.parse_declarations()?;
        self.expect_char('}')?;

        Ok(Rule {
            selectors,
            declarations,
        })
    }

    fn parse_selectors(&mut self) -> Result<Vec<Selector>, ParseError> {
        let mut selectors = Vec::new();

        loop {
            self.skip_whitespace_and_comments();
            selectors.push(self.parse_selector()?);
            self.skip_whitespace_and_comments();

            // A single rule can target multiple simple selectors separated by commas.
            if self.next_char() == Some(',') {
                self.consume_char();
                continue;
            }

            break;
        }

        Ok(selectors)
    }

    fn parse_selector(&mut self) -> Result<Selector, ParseError> {
        // A complex selector is one or more simple selectors joined by combinators.
        // Whitespace alone is the descendant combinator; `>` (optionally surrounded by
        // whitespace) is the child combinator. Compound selectors like `.a.b` (no
        // separator at all) are still rejected — we stop the chain as soon as we see
        // another simple selector that is not preceded by whitespace or `>`.
        let mut parts = vec![self.parse_simple_selector()?];
        let mut combinators = Vec::new();

        loop {
            let saved = self.pos;
            let ws_start = self.pos;
            self.consume_whitespace();
            let had_whitespace = self.pos > ws_start;

            // `>` takes precedence over whitespace: the surrounding spaces are just
            // formatting, the combinator itself is Child.
            let combinator = if self.next_char() == Some('>') {
                self.consume_char();
                self.consume_whitespace();
                Combinator::Child
            } else if had_whitespace {
                Combinator::Descendant
            } else {
                self.pos = saved;
                break;
            };

            match self.next_char() {
                Some(ch) if ch == '.' || ch == '#' || ch.is_ascii_alphabetic() || ch == '_' => {
                    parts.push(self.parse_simple_selector()?);
                    combinators.push(combinator);
                }
                _ => {
                    self.pos = saved;
                    break;
                }
            }
        }

        Ok(Selector { parts, combinators })
    }

    fn parse_simple_selector(&mut self) -> Result<SimpleSelector, ParseError> {
        let kind = match self.next_char() {
            Some('.') => {
                self.consume_char();
                SimpleSelectorKind::Class(self.parse_identifier()?)
            }
            Some('#') => {
                self.consume_char();
                SimpleSelectorKind::Id(self.parse_identifier()?)
            }
            Some(_) => SimpleSelectorKind::Tag(self.parse_identifier()?),
            None => {
                return Err(ParseError::new(
                    self.pos,
                    "unexpected end of input while parsing selector",
                ));
            }
        };

        // Optional pseudo-class suffix glued directly to the kind (e.g. `.btn:hover`).
        let pseudo = if self.next_char() == Some(':') {
            self.consume_char();
            let name = self.parse_identifier()?;
            match name.as_str() {
                "hover" => Some(PseudoClass::Hover),
                "focus" => Some(PseudoClass::Focus),
                "active" => Some(PseudoClass::Active),
                "link" => Some(PseudoClass::Link),
                "visited" => Some(PseudoClass::Visited),
                // Unknown pseudo-classes parse silently to None so the surrounding rule
                // is still applied; the selector just never matches `:unknown` cases.
                _ => None,
            }
        } else {
            None
        };

        Ok(SimpleSelector { kind, pseudo })
    }

    fn parse_declarations(&mut self) -> Result<Vec<Declaration>, ParseError> {
        let mut declarations = Vec::new();

        loop {
            self.skip_whitespace_and_comments();

            if self.eof() || self.next_char() == Some('}') {
                break;
            }

            // Try parsing a declaration; skip to the next ';' or '}' on failure.
            // Shorthands like `border-radius` expand into multiple per-corner declarations,
            // so each call may contribute more than one entry.
            let saved = self.pos;
            match self.parse_declaration() {
                Ok(decls) => declarations.extend(decls),
                Err(_) => {
                    self.pos = saved;
                    self.consume_while(|ch| ch != ';' && ch != '}');
                }
            }

            self.skip_whitespace_and_comments();
            if self.next_char() == Some(';') {
                self.consume_char();
            }
        }

        Ok(declarations)
    }

    fn parse_declaration(&mut self) -> Result<Vec<Declaration>, ParseError> {
        let name = self.parse_identifier()?;
        self.consume_whitespace();
        self.expect_char(':')?;
        self.consume_whitespace();

        if name == "border-radius" {
            return self.parse_border_radius_shorthand();
        }

        if name == "box-shadow" {
            let value = self.parse_box_shadow_value()?;
            return Ok(vec![Declaration { name, value }]);
        }

        if name == "text-shadow" {
            let value = self.parse_text_shadow_value()?;
            return Ok(vec![Declaration { name, value }]);
        }

        if name == "transform" {
            let value = self.parse_transform_value()?;
            return Ok(vec![Declaration { name, value }]);
        }

        if name == "flex" {
            return self.parse_flex_shorthand();
        }

        if name == "grid-template-columns" || name == "grid-template-rows" {
            let value = self.parse_grid_track_list()?;
            return Ok(vec![Declaration { name, value }]);
        }

        if name == "grid-column" || name == "grid-row" {
            let value = self.parse_grid_placement()?;
            return Ok(vec![Declaration { name, value }]);
        }

        if name == "grid-template-areas" {
            let value = self.parse_grid_template_areas()?;
            return Ok(vec![Declaration { name, value }]);
        }

        let value = self.parse_value()?;
        Ok(vec![Declaration { name, value }])
    }

    fn parse_grid_template_areas(&mut self) -> Result<Value, ParseError> {
        // `grid-template-areas: "a a b" "c c b" "c c b";`
        // Each row is a quoted string of whitespace-separated tokens. `.` is
        // a designated empty cell. Adjacent cells with the same name form
        // one rectangular area; the layout pass scans the map and builds a
        // bounding rectangle when an item asks for that area by name.
        let mut rows: Vec<Vec<Option<String>>> = Vec::new();
        loop {
            self.consume_whitespace();
            match self.next_char() {
                Some('"') => {}
                _ => break,
            }
            self.consume_char(); // opening "
            let body = self.consume_while(|ch| ch != '"');
            if self.next_char() == Some('"') {
                self.consume_char(); // closing "
            } else {
                return Err(ParseError::new(self.pos, "unterminated string in grid-template-areas"));
            }
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
                self.pos,
                "grid-template-areas requires at least one row string",
            ));
        }
        Ok(Value::TemplateAreas(rows))
    }

    fn parse_grid_placement(&mut self) -> Result<Value, ParseError> {
        // `grid-column: <line> [/ <line>]?`. Each side is one of:
        //   - `auto`
        //   - `<integer>` (1-based grid line; negatives skipped for now)
        //   - `span <integer>`
        // Missing `/ <line>` defaults the end side to `auto`, which the
        // layout pass interprets as span-1.
        self.consume_whitespace();
        let start = self.parse_grid_line()?;
        self.consume_whitespace();
        let end = if self.next_char() == Some('/') {
            self.consume_char();
            self.consume_whitespace();
            self.parse_grid_line()?
        } else {
            GridLine::Auto
        };
        Ok(Value::GridPlacement(GridPlacement { start, end }))
    }

    fn parse_grid_line(&mut self) -> Result<GridLine, ParseError> {
        // `auto` / `span <n>` come first because they're keyword-led.
        if matches!(self.next_char(), Some(ch) if ch.is_ascii_alphabetic()) {
            let keyword = self.parse_identifier()?;
            return match keyword.as_str() {
                "auto" => Ok(GridLine::Auto),
                "span" => {
                    self.consume_whitespace();
                    let n = self.parse_grid_line_integer()?;
                    if n == 0 {
                        return Err(ParseError::new(self.pos, "span must be >= 1"));
                    }
                    Ok(GridLine::Span(n))
                }
                other => Err(ParseError::new(
                    self.pos,
                    format!("unsupported grid-line keyword '{other}'"),
                )),
            };
        }
        let n = self.parse_grid_line_integer()?;
        if n == 0 {
            return Err(ParseError::new(self.pos, "grid line must be >= 1"));
        }
        Ok(GridLine::Index(n))
    }

    fn parse_grid_line_integer(&mut self) -> Result<u32, ParseError> {
        let digits = self.consume_while(|ch| ch.is_ascii_digit());
        digits.parse::<u32>().map_err(|_| {
            ParseError::new(
                self.pos,
                format!("invalid grid line integer '{digits}'"),
            )
        })
    }

    fn parse_grid_track_list(&mut self) -> Result<Value, ParseError> {
        // CSS `grid-template-columns: 100px 1fr 200px` — whitespace-separated
        // track sizes. Each token is either a `<length>` (resolves later in
        // layout) or a `<number>fr` (a flexible fraction). The `fr` unit only
        // makes sense inside this list, so we parse it here instead of
        // teaching the generic length parser about it.
        let mut tracks = Vec::new();
        loop {
            self.consume_whitespace();
            match self.next_char() {
                Some(';') | Some('}') | None => break,
                _ => {}
            }
            let track = self.parse_grid_track_size()?;
            tracks.push(track);
        }
        if tracks.is_empty() {
            return Err(ParseError::new(
                self.pos,
                "grid track list requires at least one track size",
            ));
        }
        Ok(Value::TrackList(tracks))
    }

    fn parse_grid_track_size(&mut self) -> Result<TrackSize, ParseError> {
        // `auto` is the only non-numeric token allowed in commit G2; bigger
        // keywords like `min-content`/`max-content` would extend this branch.
        if matches!(self.next_char(), Some(ch) if ch.is_ascii_alphabetic()) {
            let keyword = self.parse_identifier()?;
            return match keyword.as_str() {
                "auto" => Ok(TrackSize::Auto),
                other => Err(ParseError::new(
                    self.pos,
                    format!("unsupported grid track keyword '{other}'"),
                )),
            };
        }

        // Read the leading number, then peek a unit — `fr` becomes a Fraction,
        // anything else routes through the regular length unit set.
        let number_str = self.consume_while(|ch| ch.is_ascii_digit() || ch == '.');
        if number_str.is_empty() {
            return Err(ParseError::new(
                self.pos,
                "grid track size requires a numeric value",
            ));
        }
        let value = number_str.parse::<f32>().map_err(|_| {
            ParseError::new(
                self.pos,
                format!("invalid numeric value '{number_str}' in grid track"),
            )
        })?;

        if self.next_char() == Some('%') {
            self.consume_char();
            return Ok(TrackSize::Length(value, Unit::Percent));
        }

        if !matches!(self.next_char(), Some(ch) if ch.is_alphabetic()) {
            return Err(ParseError::new(
                self.pos,
                "grid track size requires a unit (px/em/rem/% or fr)",
            ));
        }

        let unit = self.parse_identifier()?;
        match unit.as_str() {
            "fr" => Ok(TrackSize::Fraction(value)),
            "px" => Ok(TrackSize::Length(value, Unit::Px)),
            "em" => Ok(TrackSize::Length(value, Unit::Em)),
            "rem" => Ok(TrackSize::Length(value, Unit::Rem)),
            other => Err(ParseError::new(
                self.pos,
                format!("unsupported grid track unit '{other}'"),
            )),
        }
    }

    fn parse_flex_shorthand(&mut self) -> Result<Vec<Declaration>, ParseError> {
        // CSS `flex` shorthand sets flex-grow / flex-shrink / flex-basis.
        // Toy support is the common forms only:
        //   flex: <number>                       → grow:<n>, shrink:1
        //   flex: <number> <number>              → grow, shrink
        //   flex: <number> <number> <length>     → grow, shrink, basis
        //   flex: <length>                       → grow:1, shrink:1, basis
        //   flex: <number> <length>              → grow, shrink:1, basis
        // Keywords (`auto`, `none`, `initial`) are not recognised — author
        // must spell out longhands or use a numeric form. Implicit basis is
        // left unset, which means flex children fall back to width-based
        // (or shrink-to-fit) sizing in the layout pass.
        let mut grow: Option<f32> = None;
        let mut shrink: Option<f32> = None;
        let mut basis: Option<Value> = None;

        for _slot in 0..3 {
            self.consume_whitespace();
            match self.next_char() {
                Some(';') | Some('}') | None => break,
                Some(ch) if !(ch.is_ascii_digit() || ch == '.' || ch == '-') => break,
                _ => {}
            }
            match self.parse_length_or_number()? {
                Value::Number(n) => {
                    if grow.is_none() {
                        grow = Some(n);
                    } else if shrink.is_none() {
                        shrink = Some(n);
                    } else {
                        return Err(ParseError::new(
                            self.pos,
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
                self.pos,
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

    fn parse_transform_value(&mut self) -> Result<Value, ParseError> {
        // CSS transform is a whitespace-separated function list; each entry is
        // one of `translate(...)`, `translateX(...)`, `translateY(...)` for now.
        // The list ends at ';' or '}'. An empty list parses as keyword `none`
        // up the stack, so we require at least one function here.
        let mut ops = Vec::new();
        loop {
            self.consume_whitespace();
            match self.next_char() {
                Some(';') | Some('}') | None => break,
                _ => {}
            }
            let name = self.parse_identifier()?;
            self.expect_char('(')?;
            let op = match name.as_str() {
                "translate" => {
                    self.consume_whitespace();
                    let x = self.parse_length_token()?;
                    self.consume_whitespace();
                    let y = if self.next_char() == Some(',') {
                        self.consume_char();
                        self.consume_whitespace();
                        let value = self.parse_length_token()?;
                        self.consume_whitespace();
                        value
                    } else {
                        // Single-arg form keeps the y component at 0.
                        0.0
                    };
                    TransformOp::Translate { x, y }
                }
                "translateX" => {
                    self.consume_whitespace();
                    let x = self.parse_length_token()?;
                    self.consume_whitespace();
                    TransformOp::Translate { x, y: 0.0 }
                }
                "translateY" => {
                    self.consume_whitespace();
                    let y = self.parse_length_token()?;
                    self.consume_whitespace();
                    TransformOp::Translate { x: 0.0, y }
                }
                "scale" => {
                    // `scale(x)` is shorthand for `scale(x, x)` (uniform scale);
                    // the two-arg form sets both axes independently.
                    self.consume_whitespace();
                    let x = self.parse_length_token()?;
                    self.consume_whitespace();
                    let y = if self.next_char() == Some(',') {
                        self.consume_char();
                        self.consume_whitespace();
                        let value = self.parse_length_token()?;
                        self.consume_whitespace();
                        value
                    } else {
                        x
                    };
                    TransformOp::Scale { x, y }
                }
                "scaleX" => {
                    self.consume_whitespace();
                    let x = self.parse_length_token()?;
                    self.consume_whitespace();
                    TransformOp::Scale { x, y: 1.0 }
                }
                "scaleY" => {
                    self.consume_whitespace();
                    let y = self.parse_length_token()?;
                    self.consume_whitespace();
                    TransformOp::Scale { x: 1.0, y }
                }
                "rotate" => {
                    self.consume_whitespace();
                    let theta = self.parse_angle_token()?;
                    self.consume_whitespace();
                    TransformOp::Rotate(theta)
                }
                other => {
                    return Err(ParseError::new(
                        self.pos,
                        format!("unsupported transform function '{other}'"),
                    ));
                }
            };
            self.expect_char(')')?;
            ops.push(op);
        }
        if ops.is_empty() {
            return Err(ParseError::new(
                self.pos,
                "transform requires at least one function",
            ));
        }
        Ok(Value::TransformList(ops))
    }

    fn parse_text_shadow_value(&mut self) -> Result<Value, ParseError> {
        // Grammar: <offset-x> <offset-y> [<blur>] [<color>]. No spread (the
        // notion doesn't apply to glyph shadows). Color defaults to opaque
        // black; blur defaults to 0 and clamps to non-negative.
        let offset_x = self.parse_length_token()?;
        self.consume_whitespace();
        let offset_y = self.parse_length_token()?;
        self.consume_whitespace();

        let blur_radius = if self.peek_starts_length() {
            let value = self.parse_length_token()?.max(0.0);
            self.consume_whitespace();
            value
        } else {
            0.0
        };

        let color = if matches!(self.next_char(), Some(';') | Some('}') | None) {
            Color {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            }
        } else {
            match self.parse_value()? {
                Value::Color(color) => color,
                other => {
                    return Err(ParseError::new(
                        self.pos,
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

    fn parse_box_shadow_value(&mut self) -> Result<Value, ParseError> {
        // Grammar (MVP): <offset-x> <offset-y> [<blur>] [<spread>] [<color>].
        // We greedily consume up to four leading lengths (offset-x, offset-y,
        // blur, spread) and then anything left over is the color. `inset` and
        // multi-shadow comma lists are out of scope here.
        let offset_x = self.parse_length_token()?;
        self.consume_whitespace();
        let offset_y = self.parse_length_token()?;
        self.consume_whitespace();

        let mut blur_radius = 0.0;
        let mut spread_radius = 0.0;
        for slot in 0..2 {
            if !self.peek_starts_length() {
                break;
            }
            let value = self.parse_length_token()?;
            if slot == 0 {
                blur_radius = value.max(0.0);
            } else {
                spread_radius = value;
            }
            self.consume_whitespace();
        }

        let color = if matches!(self.next_char(), Some(';') | Some('}') | None) {
            // Default shadow color: opaque black, matching the common toy use.
            Color {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            }
        } else {
            match self.parse_value()? {
                Value::Color(color) => color,
                other => {
                    return Err(ParseError::new(
                        self.pos,
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

    fn parse_angle_token(&mut self) -> Result<f32, ParseError> {
        // CSS rotate() takes an `<angle>`: a number plus a unit (deg, rad,
        // turn, grad). The toy supports all four and converts to radians so
        // the renderer can call `.sin_cos()` directly without re-checking
        // units later.
        self.consume_whitespace();
        let negative = if self.next_char() == Some('-') {
            self.consume_char();
            true
        } else {
            false
        };
        let number = self.consume_while(|ch| ch.is_ascii_digit() || ch == '.');
        let mut value = number
            .parse::<f32>()
            .map_err(|_| ParseError::new(self.pos, format!("invalid angle '{number}'")))?;
        if negative {
            value = -value;
        }
        let unit = match self.next_char() {
            Some(ch) if ch.is_alphabetic() => self.parse_identifier()?,
            // CSS spec only allows a unitless 0; anything else needs a unit,
            // but `0` with no unit is common enough to accept defensively.
            _ => String::new(),
        };
        let radians = match unit.as_str() {
            "deg" => value * std::f32::consts::PI / 180.0,
            "rad" => value,
            "turn" => value * std::f32::consts::TAU,
            "grad" => value * std::f32::consts::PI / 200.0,
            "" if value == 0.0 => 0.0,
            other => {
                return Err(ParseError::new(
                    self.pos,
                    format!("unsupported angle unit '{other}'"),
                ));
            }
        };
        Ok(radians)
    }

    fn parse_length_token(&mut self) -> Result<f32, ParseError> {
        // Reuse the generic value parser so a leading minus / unitless number
        // path is handled exactly like elsewhere, then narrow the result to a
        // numeric component.
        let value = self.parse_value()?;
        match value {
            Value::Length(v, _) => Ok(v),
            Value::Number(v) => Ok(v),
            other => Err(ParseError::new(
                self.pos,
                format!("expected a length token, got {other:?}"),
            )),
        }
    }

    fn peek_starts_length(&self) -> bool {
        // Token that can start a numeric value: digit, decimal point, or a
        // minus sign followed by either of those.
        match self.next_char() {
            Some(ch) if ch.is_ascii_digit() || ch == '.' => true,
            Some('-') => {
                let mut chars = self.input[self.pos..].chars();
                chars.next();
                matches!(chars.next(), Some(ch) if ch.is_ascii_digit() || ch == '.')
            }
            _ => false,
        }
    }

    fn parse_border_radius_shorthand(&mut self) -> Result<Vec<Declaration>, ParseError> {
        // CSS allows 1-4 length tokens. Per spec: 1=all, 2=tl/br + tr/bl,
        // 3=tl + tr/bl + br, 4=tl + tr + br + bl.
        let mut lengths = Vec::new();
        loop {
            self.consume_whitespace();
            match self.next_char() {
                Some(ch) if ch.is_ascii_digit() || ch == '.' => {}
                _ => break,
            }
            lengths.push(self.parse_length_or_number()?);
            if lengths.len() == 4 {
                break;
            }
        }

        if lengths.is_empty() {
            return Err(ParseError::new(
                self.pos,
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

    fn parse_value(&mut self) -> Result<Value, ParseError> {
        match self.next_char() {
            Some('#') => self.parse_hex_color(),
            Some(ch) if ch.is_ascii_digit() => self.parse_length_or_number(),
            // A leading minus only counts as a numeric sign when an actual digit
            // (or decimal point) follows; otherwise it is the start of an
            // identifier (e.g. CSS custom properties like `--color`).
            Some('-') if self.peeks_negative_number() => {
                self.consume_char();
                Ok(negate_numeric(self.parse_length_or_number()?))
            }
            // Identifier-shaped values cover plain keywords (block, auto, ...), named
            // colors, and functional color notations like rgb()/rgba(). The functional
            // form is recognised when an opening paren follows the identifier name.
            Some(_) => {
                let ident = self.parse_identifier()?;
                if self.next_char() == Some('(') {
                    if ident.eq_ignore_ascii_case("rgb") || ident.eq_ignore_ascii_case("rgba") {
                        return self.parse_rgb_function(ident.eq_ignore_ascii_case("rgba"));
                    }
                    if ident.eq_ignore_ascii_case("linear-gradient") {
                        return self.parse_linear_gradient();
                    }
                    if ident.eq_ignore_ascii_case("radial-gradient") {
                        return self.parse_radial_gradient();
                    }
                }
                Ok(named_color(&ident)
                    .map(Value::Color)
                    .unwrap_or(Value::Keyword(ident)))
            }
            None => Err(ParseError::new(
                self.pos,
                "unexpected end of input while parsing value",
            )),
        }
    }

    fn peeks_negative_number(&self) -> bool {
        let mut chars = self.input[self.pos..].chars();
        let first = chars.next();
        let second = chars.next();
        first == Some('-') && matches!(second, Some(ch) if ch.is_ascii_digit() || ch == '.')
    }

    fn parse_rgb_function(&mut self, has_alpha: bool) -> Result<Value, ParseError> {
        // We only handle the legacy comma-separated form (`rgb(r, g, b)` and
        // `rgba(r, g, b, a)`). Modern whitespace + slash syntax and percentage
        // components are intentionally out of scope for now.
        self.expect_char('(')?;
        let r = self.parse_color_byte()?;
        self.expect_comma()?;
        let g = self.parse_color_byte()?;
        self.expect_comma()?;
        let b = self.parse_color_byte()?;
        let a = if has_alpha {
            self.expect_comma()?;
            // Alpha is authored as a 0..1 float in CSS; we store as u8 by scaling.
            let alpha = self.parse_unsigned_number()?;
            (alpha.clamp(0.0, 1.0) * 255.0).round() as u8
        } else {
            255
        };
        self.consume_whitespace();
        self.expect_char(')')?;

        Ok(Value::Color(Color { r, g, b, a }))
    }

    fn parse_linear_gradient(&mut self) -> Result<Value, ParseError> {
        // Supported forms (MVP):
        //   linear-gradient(<color> [<%>]?, <color> [<%>]? [, ...])
        //   linear-gradient(to top|bottom|left|right, <stops...>)
        // Angle (`45deg`) and corner directions (`to top left`) are deferred.
        self.expect_char('(')?;
        self.consume_whitespace();

        let direction = if self.starts_with("to ") || self.starts_with("to\t") {
            // Eat "to" identifier, then read a side keyword. A direction prefix
            // must be followed by a comma before the first color stop.
            let _to = self.parse_identifier()?;
            self.consume_whitespace();
            let side = self.parse_identifier()?;
            let dir = match side.as_str() {
                "top" => GradientDirection::ToTop,
                "bottom" => GradientDirection::ToBottom,
                "left" => GradientDirection::ToLeft,
                "right" => GradientDirection::ToRight,
                other => {
                    return Err(ParseError::new(
                        self.pos,
                        format!("unsupported gradient direction 'to {other}'"),
                    ));
                }
            };
            self.consume_whitespace();
            self.expect_char(',')?;
            dir
        } else {
            GradientDirection::ToBottom
        };

        let stops = self.parse_gradient_stops("linear-gradient")?;
        Ok(Value::Gradient(Gradient {
            kind: GradientKind::Linear(direction),
            stops,
        }))
    }

    fn parse_radial_gradient(&mut self) -> Result<Value, ParseError> {
        // MVP form: `radial-gradient(<stops>)`. The renderer treats it as an
        // ellipse sized to the farthest corner, centered in the box. Shape /
        // size / position prefixes (`circle at 30% 70%`, etc.) are deferred.
        self.expect_char('(')?;
        self.consume_whitespace();
        let stops = self.parse_gradient_stops("radial-gradient")?;
        Ok(Value::Gradient(Gradient {
            kind: GradientKind::Radial,
            stops,
        }))
    }

    fn parse_gradient_stops(&mut self, label: &str) -> Result<Vec<ColorStop>, ParseError> {
        // Shared by linear and radial: comma-separated `<color> [<%>]?` pairs
        // until the closing paren. Both gradients require at least two stops.
        let mut stops = Vec::new();
        loop {
            self.consume_whitespace();
            stops.push(self.parse_color_stop()?);
            self.consume_whitespace();
            match self.next_char() {
                Some(',') => {
                    self.consume_char();
                }
                Some(')') => break,
                _ => {
                    return Err(ParseError::new(
                        self.pos,
                        format!("expected ',' or ')' in {label}"),
                    ));
                }
            }
        }
        self.expect_char(')')?;
        if stops.len() < 2 {
            return Err(ParseError::new(
                self.pos,
                format!("{label} requires at least two color stops"),
            ));
        }
        Ok(stops)
    }

    fn parse_color_stop(&mut self) -> Result<ColorStop, ParseError> {
        // A stop is a color value, optionally followed by a percentage
        // position. We delegate to parse_value so anything that would parse
        // as a color elsewhere (`#fff`, `rgb()`, named, etc.) works here too.
        let color = match self.parse_value()? {
            Value::Color(color) => color,
            other => {
                return Err(ParseError::new(
                    self.pos,
                    format!("expected a color in gradient stop, got {other:?}"),
                ));
            }
        };
        self.consume_whitespace();
        let position = if matches!(self.next_char(), Some(ch) if ch.is_ascii_digit() || ch == '.') {
            let value = self.parse_unsigned_number()?;
            if self.next_char() == Some('%') {
                self.consume_char();
                Some((value / 100.0).clamp(0.0, 1.0))
            } else {
                return Err(ParseError::new(
                    self.pos,
                    "gradient stop position must be a percentage",
                ));
            }
        } else {
            None
        };
        Ok(ColorStop { color, position })
    }

    fn parse_color_byte(&mut self) -> Result<u8, ParseError> {
        let value = self.parse_unsigned_number()?;
        Ok(value.clamp(0.0, 255.0).round() as u8)
    }

    fn parse_unsigned_number(&mut self) -> Result<f32, ParseError> {
        self.consume_whitespace();
        let number = self.consume_while(|ch| ch.is_ascii_digit() || ch == '.');
        number.parse::<f32>().map_err(|_| {
            ParseError::new(
                self.pos,
                format!("invalid numeric component '{number}' in color"),
            )
        })
    }

    fn expect_comma(&mut self) -> Result<(), ParseError> {
        self.consume_whitespace();
        self.expect_char(',')
    }

    fn parse_hex_color(&mut self) -> Result<Value, ParseError> {
        self.expect_char('#')?;
        let hex = self.consume_while(|ch| ch.is_ascii_hexdigit());

        let (r, g, b) = match hex.len() {
            3 => {
                let chars: Vec<char> = hex.chars().collect();
                (
                    Self::expand_hex(chars[0])?,
                    Self::expand_hex(chars[1])?,
                    Self::expand_hex(chars[2])?,
                )
            }
            6 => (
                Self::parse_hex_pair(&hex[0..2])?,
                Self::parse_hex_pair(&hex[2..4])?,
                Self::parse_hex_pair(&hex[4..6])?,
            ),
            _ => {
                return Err(ParseError::new(
                    self.pos,
                    "hex colors must use either 3 or 6 digits",
                ));
            }
        };

        Ok(Value::Color(Color { r, g, b, a: 255 }))
    }

    fn parse_length_or_number(&mut self) -> Result<Value, ParseError> {
        let number = self.consume_while(|ch| ch.is_ascii_digit() || ch == '.');
        let value = number.parse::<f32>().map_err(|_| {
            ParseError::new(
                self.pos,
                format!("invalid numeric value '{number}' in declaration"),
            )
        })?;

        // Percent uses a non-identifier suffix, so check it before falling through to the
        // alphabetic unit lookup.
        if self.next_char() == Some('%') {
            self.consume_char();
            return Ok(Value::Length(value, Unit::Percent));
        }

        // No alphabetic unit follows → unitless number. This is what
        // `z-index: 5`, `line-height: 1.5`, etc. produce.
        if !matches!(self.next_char(), Some(ch) if ch.is_alphabetic()) {
            return Ok(Value::Number(value));
        }

        let unit = self.parse_identifier()?;
        match unit.as_str() {
            "px" => Ok(Value::Length(value, Unit::Px)),
            "em" => Ok(Value::Length(value, Unit::Em)),
            "rem" => Ok(Value::Length(value, Unit::Rem)),
            // Unsupported units fall back to plain keywords instead of hard-failing.
            _ => Ok(Value::Keyword(format!("{value}{unit}"))),
        }
    }

    fn parse_identifier(&mut self) -> Result<String, ParseError> {
        let ident = self.consume_while(|ch| ch.is_alphanumeric() || ch == '-' || ch == '_');
        if ident.is_empty() {
            Err(ParseError::new(self.pos, "expected identifier"))
        } else {
            Ok(ident)
        }
    }

    fn consume_whitespace(&mut self) {
        self.consume_while(char::is_whitespace);
    }

    fn consume_while<F>(&mut self, test: F) -> String
    where
        F: Fn(char) -> bool,
    {
        let mut result = String::new();

        while let Some(ch) = self.next_char() {
            if !test(ch) {
                break;
            }

            result.push(self.consume_char().expect("character must exist"));
        }

        result
    }

    fn eof(&self) -> bool {
        self.pos >= self.input.len()
    }

    fn next_char(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn consume_char(&mut self) -> Option<char> {
        let current = self.next_char()?;
        self.pos += current.len_utf8();
        Some(current)
    }

    fn expect_char(&mut self, expected: char) -> Result<(), ParseError> {
        match self.consume_char() {
            Some(actual) if actual == expected => Ok(()),
            Some(actual) => Err(ParseError::new(
                self.pos,
                format!("expected '{expected}', found '{actual}'"),
            )),
            None => Err(ParseError::new(
                self.pos,
                format!("expected '{expected}', found end of input"),
            )),
        }
    }

    fn parse_hex_pair(pair: &str) -> Result<u8, ParseError> {
        u8::from_str_radix(pair, 16)
            .map_err(|_| ParseError::new(0, format!("invalid hex color pair '{pair}'")))
    }

    fn expand_hex(value: char) -> Result<u8, ParseError> {
        let mut pair = String::with_capacity(2);
        pair.push(value);
        pair.push(value);
        Self::parse_hex_pair(&pair)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BoxShadow, Color, Combinator, GradientDirection, GradientKind, GridLine, PseudoClass,
        Selector, SimpleSelector, SimpleSelectorKind, TextShadow, TrackSize, TransformOp, Unit,
        Value, parse,
    };

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
        assert_eq!(
            stylesheet.rules[0].selectors,
            vec![
                Selector::simple(SimpleSelector::tag("h1")),
                Selector::simple(SimpleSelector::class("title")),
            ]
        );
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
        assert_eq!(
            stylesheet.rules[1].selectors,
            vec![Selector::simple(SimpleSelector::id("app"))]
        );
    }

    #[test]
    fn parses_descendant_selector_chain() {
        let stylesheet = parse(".outer .inner { color: red; }").unwrap();
        let selector = &stylesheet.rules[0].selectors[0];

        // Whitespace-separated simple selectors collapse into a single descendant chain
        // ordered left-to-right (outer ancestor first, target last).
        assert_eq!(
            selector.parts,
            vec![
                SimpleSelector::class("outer"),
                SimpleSelector::class("inner"),
            ]
        );
    }

    #[test]
    fn parses_hover_pseudo_class_attached_to_simple_selector() {
        let stylesheet = parse(".btn:hover { color: red; }").unwrap();
        let part = &stylesheet.rules[0].selectors[0].parts[0];

        assert_eq!(part.kind, SimpleSelectorKind::Class("btn".into()));
        assert_eq!(part.pseudo, Some(PseudoClass::Hover));
    }

    #[test]
    fn pseudo_class_carries_through_descendant_chain() {
        let stylesheet = parse(".outer .item:hover { color: red; }").unwrap();
        let parts = &stylesheet.rules[0].selectors[0].parts;

        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].pseudo, None);
        assert_eq!(parts[1].kind, SimpleSelectorKind::Class("item".into()));
        assert_eq!(parts[1].pseudo, Some(PseudoClass::Hover));
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

        let focus_part = &stylesheet.rules[0].selectors[0].parts[0];
        let active_part = &stylesheet.rules[1].selectors[0].parts[0];

        assert_eq!(focus_part.pseudo, Some(PseudoClass::Focus));
        assert_eq!(active_part.pseudo, Some(PseudoClass::Active));
    }

    #[test]
    fn parses_link_and_visited_pseudo_classes() {
        // Anchor link/visited pseudos are common enough on real pages
        // (HN's `a:link { color: black }` is a representative example)
        // that the parser needs to recognise them as their own variants
        // rather than collapsing to the bare `a` selector — otherwise
        // the visited rule would source-order-win over the link rule
        // and every anchor would render in the visited colour.
        let stylesheet = parse(
            r#"
                a:link { color: red; }
                a:visited { color: blue; }
            "#,
        )
        .unwrap();

        let link_part = &stylesheet.rules[0].selectors[0].parts[0];
        let visited_part = &stylesheet.rules[1].selectors[0].parts[0];

        assert_eq!(link_part.pseudo, Some(PseudoClass::Link));
        assert_eq!(visited_part.pseudo, Some(PseudoClass::Visited));
    }

    #[test]
    fn unknown_pseudo_class_falls_back_to_no_pseudo() {
        // The parser stays permissive: an unknown pseudo just clears the slot rather
        // than failing the whole rule. The selector then matches its non-pseudo form.
        let stylesheet = parse(".btn:totally-fake { color: red; }").unwrap();
        let part = &stylesheet.rules[0].selectors[0].parts[0];

        assert_eq!(part.kind, SimpleSelectorKind::Class("btn".into()));
        assert_eq!(part.pseudo, None);
    }

    #[test]
    fn parses_child_combinator_with_optional_surrounding_whitespace() {
        // Both `.a > .b` and `.a>.b` should parse the same way.
        let with_spaces = parse(".outer > .inner { color: red; }").unwrap();
        let no_spaces = parse(".outer>.inner { color: red; }").unwrap();

        let expected_parts = vec![
            SimpleSelector::class("outer"),
            SimpleSelector::class("inner"),
        ];
        let expected_combinators = vec![Combinator::Child];

        assert_eq!(with_spaces.rules[0].selectors[0].parts, expected_parts);
        assert_eq!(
            with_spaces.rules[0].selectors[0].combinators,
            expected_combinators
        );
        assert_eq!(no_spaces.rules[0].selectors[0].parts, expected_parts);
        assert_eq!(
            no_spaces.rules[0].selectors[0].combinators,
            expected_combinators
        );
    }

    #[test]
    fn parses_mixed_descendant_and_child_combinators() {
        let stylesheet = parse("nav ul > li { display: block; }").unwrap();
        let selector = &stylesheet.rules[0].selectors[0];

        assert_eq!(
            selector.parts,
            vec![
                SimpleSelector::tag("nav"),
                SimpleSelector::tag("ul"),
                SimpleSelector::tag("li"),
            ]
        );
        assert_eq!(
            selector.combinators,
            vec![Combinator::Descendant, Combinator::Child]
        );
    }

    #[test]
    fn descendant_chain_supports_three_levels() {
        let stylesheet = parse("nav ul li { display: block; }").unwrap();
        assert_eq!(
            stylesheet.rules[0].selectors[0].parts,
            vec![
                SimpleSelector::tag("nav"),
                SimpleSelector::tag("ul"),
                SimpleSelector::tag("li"),
            ]
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

        // 2-value shorthand: first is tl/br, second is tr/bl.
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

        // 3-value shorthand: tl, tr/bl, br.
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

        // Shorthand still expands first; the explicit property follows and wins on cascade.
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
        // Unknown named color (SkyBlue not in our shipped subset) falls back to a keyword
        // so the parser stays permissive.
        assert_eq!(
            stylesheet.rules[1].declarations[0].value,
            Value::Keyword("SkyBlue".into())
        );
        // `cyan` is an alias for `aqua` — case-insensitive lookup picks it up.
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
        // 0.5 alpha rounds to 128/255 (0.5 * 255 = 127.5 -> 128).
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

        // The malformed "color red" declaration is skipped; valid ones are kept.
        assert_eq!(stylesheet.rules.len(), 1);
        assert_eq!(stylesheet.rules[0].declarations.len(), 1);
        assert_eq!(stylesheet.rules[0].declarations[0].name, "font-size");
    }

    #[test]
    fn parses_unitless_integer_as_number() {
        // `z-index: 5` has no unit — historically this errored; now it should
        // produce a `Value::Number`.
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
        // The same minus-prefix path also has to handle properties like
        // `margin-left: -10px` once we get to negative offsets/positions.
        let stylesheet = parse(".a { margin-left: -10px; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;

        assert_eq!(decls[0].value, Value::Length(-10.0, Unit::Px));
    }

    #[test]
    fn parses_unitless_decimal_as_number() {
        // `line-height: 1.5` is the canonical unitless-decimal use case.
        let stylesheet = parse(".a { line-height: 1.5; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;

        assert_eq!(decls[0].value, Value::Number(1.5));
    }

    #[test]
    fn parses_linear_gradient_default_direction_and_auto_stops() {
        // Without `to <side>`, the gradient runs top → bottom. Stops without
        // explicit positions stay as `None` so the renderer can distribute.
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
        // `to right` rotates the axis horizontally; the parser preserves the
        // direction without changing stop ordering.
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
        // `5px 10px 15px 2px black` populates every component in order.
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
        // Just two offsets — blur/spread default to 0, color defaults to opaque black.
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
        // `2px 3px 4px red` populates every component in order.
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
        // Just two offsets — blur defaults to 0, color defaults to black.
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
        // MVP `radial-gradient(<stops>)` — no shape/size/position prefix.
        // Should land in `Value::Gradient` with `GradientKind::Radial`.
        let stylesheet = parse(".a { background-image: radial-gradient(red, blue); }").unwrap();
        let gradient = match &stylesheet.rules[0].declarations[0].value {
            Value::Gradient(gradient) => gradient,
            other => panic!("expected Gradient, got {other:?}"),
        };
        assert_eq!(gradient.kind, GradientKind::Radial);
        assert_eq!(gradient.stops.len(), 2);
    }

    #[test]
    fn parses_transform_translate_two_args() {
        // `translate(10px, 20px)` lands as a single-op list with both axes set.
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
        // translateX/translateY are sugar for the corresponding 1-axis translate.
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
        // `scale(2)` is shorthand for `scale(2, 2)` — uniform scale on both axes.
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
        // 45deg → π/4 rad. The `rad` form is passed through verbatim.
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
                // -0.25 turn = -π/2.
                assert!((theta + std::f32::consts::FRAC_PI_2).abs() < 1e-4);
            }
            other => panic!("expected single Rotate, got {other:?}"),
        }
    }

    #[test]
    fn parses_linear_gradient_with_explicit_stop_positions() {
        // `red 0%, yellow 50%, blue 100%` should preserve every stop's
        // percentage as a 0..1 float, no auto-distribution.
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
        // A bare number with no unit is ambiguous (`5` could be 5px or 5fr)
        // so the parser refuses rather than guessing.
        let stylesheet = parse(".g { grid-template-columns: 5; }");
        // The parser is tolerant — it returns Ok with the rule dropped or kept
        // empty. Just assert the bad declaration didn't sneak through as a
        // TrackList by the size matching the dropped state.
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
