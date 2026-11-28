// CSS grid value parsing — the longhand parsers for `grid-template-areas`,
// `grid-template-columns/rows` (track lists), and the per-item placement
// shorthand (`grid-column` / `grid-row`). The cascade routes these from
// `parse_declaration_value`; `length_with_unit` / `parse_function`
// helpers stay in mod.rs because they're cross-cutting.

use cssparser::{Parser as CssParser, Token};

use super::calc::Calc;
use super::error::{convert_basic_error_at, token_error};
use super::{
    AutoRepeat, Declaration, GridLine, GridPlacement, ParseError, TrackBreadth, TrackList,
    TrackSize, Unit, Value,
};

pub(super) fn parse_grid_template_areas<'i>(
    input: &mut CssParser<'i>,
) -> Result<Value, ParseError> {
    if input.try_parse(|i| i.expect_ident_matching("none")).is_ok() {
        return Ok(Value::Keyword("none".into()));
    }
    let rows = parse_area_rows(input, |_| Ok(()))?;
    if rows.is_empty() {
        return Err(ParseError::new(
            input.position().byte_index(),
            "grid-template-areas requires at least one row string",
        ));
    }
    Ok(Value::TemplateAreas(rows))
}

/// Row strings of a template, each optionally followed by whatever
/// `after_row` reads (the `grid-template` shorthand's row size).
fn parse_area_rows<'i>(
    input: &mut CssParser<'i>,
    mut after_row: impl FnMut(&mut CssParser<'i>) -> Result<(), ParseError>,
) -> Result<Vec<Vec<Option<String>>>, ParseError> {
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
        after_row(input)?;
    }
    Ok(rows)
}

/// `grid-template` → `grid-template-rows` / `-columns` / `-areas`
/// (css-grid-1 §7.4). Three forms: `none`; `<rows> / <columns>` (areas
/// reset to `none`); and `[ <string> <track-size>? ]+ [ / <columns> ]?`.
/// Line names and `repeat()` are read like the longhands (step 5b).
pub(super) fn parse_grid_template_shorthand<'i>(
    input: &mut CssParser<'i>,
) -> Result<Vec<Declaration>, ParseError> {
    let none = || Value::Keyword("none".into());
    let declarations = |rows: Value, columns: Value, areas: Value| {
        vec![
            Declaration {
                name: "grid-template-rows".into(),
                value: rows,
                important: false,
            },
            Declaration {
                name: "grid-template-columns".into(),
                value: columns,
                important: false,
            },
            Declaration {
                name: "grid-template-areas".into(),
                value: areas,
                important: false,
            },
        ]
    };
    if input
        .try_parse(|i| {
            i.expect_ident_matching("none")?;
            i.expect_exhausted()
        })
        .is_ok()
    {
        return Ok(declarations(none(), none(), none()));
    }
    input.skip_whitespace();
    let starts_with_string = {
        let probe = input.state();
        let is_string = matches!(input.next(), Ok(Token::QuotedString(_)));
        input.reset(&probe);
        is_string
    };
    if starts_with_string {
        let mut row_sizes = Vec::new();
        let mut row_calcs = Vec::new();
        let areas = parse_area_rows(input, |input| {
            input.skip_whitespace();
            let probe = input.state();
            let size = match input.next() {
                Ok(Token::QuotedString(_)) | Ok(Token::Delim('/')) | Err(_) => None,
                Ok(_) => {
                    input.reset(&probe);
                    Some(parse_grid_track_size(input, &mut row_calcs)?)
                }
            };
            if size.is_none() {
                input.reset(&probe);
            }
            row_sizes.push(size.unwrap_or(TrackSize::Auto));
            Ok(())
        })?;
        let columns = if input.try_parse(|i| i.expect_delim('/')).is_ok() {
            let columns = parse_track_sizes(input)?;
            if columns.tracks.is_empty() && columns.auto_repeat.is_none() {
                return Err(ParseError::new(
                    input.position().byte_index(),
                    "grid-template needs columns after '/'",
                ));
            }
            Value::TrackList(std::sync::Arc::new(columns))
        } else {
            none()
        };
        input
            .expect_exhausted()
            .map_err(|err| convert_basic_error_at(input.position().byte_index(), err))?;
        return Ok(declarations(
            Value::TrackList(std::sync::Arc::new(TrackList {
                tracks: row_sizes,
                calcs: row_calcs,
                ..TrackList::default()
            })),
            columns,
            Value::TemplateAreas(areas),
        ));
    }
    let rows = parse_track_list_or_none(input)?;
    input
        .expect_delim('/')
        .map_err(|err| convert_basic_error_at(input.position().byte_index(), err))?;
    let columns = parse_track_list_or_none(input)?;
    input
        .expect_exhausted()
        .map_err(|err| convert_basic_error_at(input.position().byte_index(), err))?;
    Ok(declarations(rows, columns, none()))
}

/// One side of `<rows> / <columns>`: `none` or at least one track size.
fn parse_track_list_or_none<'i>(input: &mut CssParser<'i>) -> Result<Value, ParseError> {
    if input.try_parse(|i| i.expect_ident_matching("none")).is_ok() {
        return Ok(Value::Keyword("none".into()));
    }
    let tracks = parse_track_sizes(input)?;
    if tracks.tracks.is_empty() && tracks.auto_repeat.is_none() {
        return Err(ParseError::new(
            input.position().byte_index(),
            "grid track list requires at least one track size",
        ));
    }
    Ok(Value::TrackList(std::sync::Arc::new(tracks)))
}

pub(super) fn parse_grid_placement<'i>(input: &mut CssParser<'i>) -> Result<Value, ParseError> {
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

fn parse_grid_line<'i>(input: &mut CssParser<'i>) -> Result<GridLine, ParseError> {
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

fn parse_grid_line_integer<'i>(input: &mut CssParser<'i>) -> Result<u32, ParseError> {
    let pos = input.position().byte_index();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(pos, err))?
        .clone();
    match token {
        Token::Number {
            int_value: Some(n), ..
        } if n >= 0 => Ok(n as u32),
        Token::Number { value, .. } if value >= 0.0 => Ok(value as u32),
        other => Err(token_error(input, &other, "invalid grid line integer")),
    }
}

pub(super) fn parse_grid_track_list<'i>(input: &mut CssParser<'i>) -> Result<Value, ParseError> {
    let value = parse_track_list_or_none(input)?;
    input
        .expect_exhausted()
        .map_err(|err| convert_basic_error_at(input.position().byte_index(), err))?;
    Ok(value)
}

/// What a track list is being built into (step 5b): its tracks, the names
/// of each line so far, the auto repeat, the calc breadths.
#[derive(Default)]
struct ListBuilder {
    list: TrackList,
}

impl ListBuilder {
    /// The names of the line before the next track.
    fn line_names(&mut self) -> &mut Vec<String> {
        let line = self.list.tracks.len();
        if self.list.names.len() <= line {
            self.list.names.resize(line + 1, Vec::new());
        }
        &mut self.list.names[line]
    }

    fn push(&mut self, track: TrackSize) -> bool {
        if self.list.tracks.len() >= TrackList::MAX_TRACKS {
            return false;
        }
        self.list.tracks.push(track);
        true
    }

    fn finish(mut self) -> TrackList {
        if self.list.names.iter().all(Vec::is_empty) {
            self.list.names.clear();
        } else {
            let lines = self.list.tracks.len() + 1;
            self.list.names.resize(lines, Vec::new());
        }
        self.list
    }
}

/// Track sizes up to the end of the input or a `/` (left unread), with
/// line names and `repeat()`.
fn parse_track_sizes<'i>(input: &mut CssParser<'i>) -> Result<TrackList, ParseError> {
    let mut builder = ListBuilder::default();
    loop {
        input.skip_whitespace();
        let probe = input.state();
        let pos = input.position().byte_index();
        match input.next() {
            Err(_) => break,
            Ok(Token::Delim('/')) => {
                input.reset(&probe);
                break;
            }
            Ok(Token::SquareBracketBlock) => {
                let names = parse_line_names(input, pos)?;
                builder.line_names().extend(names);
            }
            Ok(Token::Function(name)) if name.eq_ignore_ascii_case("repeat") => {
                parse_repeat(input, pos, &mut builder)?;
            }
            Ok(_) => {
                input.reset(&probe);
                let track = parse_grid_track_size(input, &mut builder.list.calcs)?;
                builder.push(track);
            }
        }
    }
    Ok(builder.finish())
}

/// `[name name …]` (the parser is just past the `[`).
fn parse_line_names<'i>(input: &mut CssParser<'i>, pos: usize) -> Result<Vec<String>, ParseError> {
    input
        .parse_nested_block(|block| {
            let mut names = Vec::new();
            while let Ok(token) = block.next() {
                match token {
                    Token::Ident(name) => names.push(name.to_string()),
                    _ => {
                        return Err(cssparser::ParseError::<ParseError>::custom(
                            ParseError::new(pos, ""),
                        ));
                    }
                }
            }
            Ok(names)
        })
        .map_err(|_| ParseError::new(pos, "invalid grid line names"))
}

/// `repeat(<n> | auto-fill | auto-fit, <tracks>)` (the parser is just past
/// `repeat(`). A count repeats tracks and names now, up to
/// `TrackList::MAX_TRACKS`; an auto repeat is kept for layout.
fn parse_repeat<'i>(
    input: &mut CssParser<'i>,
    pos: usize,
    builder: &mut ListBuilder,
) -> Result<(), ParseError> {
    let invalid = |why: &str| ParseError::new(pos, format!("repeat(): {why}"));
    enum Count {
        Times(usize),
        Auto { fit: bool },
    }
    let calcs = &mut builder.list.calcs;
    let (count, inner) = input
        .parse_nested_block(|block| {
            let count = match block.next()?.clone() {
                Token::Number {
                    int_value: Some(n), ..
                } if n >= 1 => Count::Times(n as usize),
                Token::Ident(word) if word.eq_ignore_ascii_case("auto-fill") => {
                    Count::Auto { fit: false }
                }
                Token::Ident(word) if word.eq_ignore_ascii_case("auto-fit") => {
                    Count::Auto { fit: true }
                }
                _ => return Err(cssparser::ParseError::custom(invalid("bad count"))),
            };
            block.expect_comma()?;
            // The repeated tracks and the names around each.
            let mut inner = ListBuilder::default();
            loop {
                block.skip_whitespace();
                let probe = block.state();
                let at = block.position().byte_index();
                match block.next() {
                    Err(_) => break,
                    Ok(Token::SquareBracketBlock) => {
                        let names =
                            parse_line_names(block, at).map_err(cssparser::ParseError::custom)?;
                        inner.line_names().extend(names);
                    }
                    Ok(_) => {
                        block.reset(&probe);
                        let track = parse_grid_track_size(block, calcs)
                            .map_err(cssparser::ParseError::custom)?;
                        inner.push(track);
                    }
                }
            }
            Ok((count, inner.finish()))
        })
        .map_err(|_: cssparser::ParseError<ParseError>| invalid("invalid"))?;
    if inner.tracks.is_empty() {
        return Err(invalid("no tracks"));
    }
    match count {
        Count::Times(n) => {
            for _ in 0..n {
                for (i, track) in inner.tracks.iter().enumerate() {
                    if let Some(names) = inner.names.get(i) {
                        builder.line_names().extend(names.iter().cloned());
                    }
                    if !builder.push(*track) {
                        return Ok(());
                    }
                }
                if let Some(names) = inner.names.last() {
                    builder.line_names().extend(names.iter().cloned());
                }
            }
            Ok(())
        }
        Count::Auto { fit } => {
            if builder.list.auto_repeat.is_some() {
                return Err(invalid("one auto repeat per list"));
            }
            if !inner.tracks.iter().all(is_fixed_size) {
                return Err(invalid("auto repeat needs definite sizes"));
            }
            builder.list.auto_repeat = Some(AutoRepeat {
                fit,
                at: builder.list.tracks.len(),
                tracks: inner.tracks,
            });
            Ok(())
        }
    }
}

/// `<fixed-size>` (css-grid-1 §7.2.3.1): a definite length, or a
/// `minmax()` with one definite side and an inflexible minimum.
fn is_fixed_size(track: &TrackSize) -> bool {
    let fixed = |b: &TrackBreadth| matches!(b, TrackBreadth::Length(..) | TrackBreadth::Calc(_));
    match track {
        TrackSize::Length(..) | TrackSize::Calc(_) => true,
        TrackSize::MinMax(min, max) => {
            fixed(min) || (fixed(max) && !matches!(min, TrackBreadth::Fraction(_)))
        }
        _ => false,
    }
}

/// `<track-size>`: a breadth (`1fr` is a track size on its own),
/// `minmax(<min>, <max>)`, `fit-content(<length-percentage>)` or a math
/// function (step 5b). Calc breadths that keep a `%` go to `calcs`.
fn parse_grid_track_size<'i>(
    input: &mut CssParser<'i>,
    calcs: &mut Vec<Calc>,
) -> Result<TrackSize, ParseError> {
    input.skip_whitespace();
    let pos = input.position().byte_index();
    let probe = input.state();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(pos, err))?
        .clone();
    if let Token::Function(name) = &token {
        let lower = name.to_ascii_lowercase();
        if lower == "minmax" {
            return input
                .parse_nested_block(|input| {
                    let min = parse_track_breadth(input, calcs)?;
                    input.expect_comma()?;
                    let max = parse_track_breadth(input, calcs)?;
                    input.expect_exhausted()?;
                    if matches!(min, TrackBreadth::Fraction(_)) {
                        return Err(cssparser::ParseError::custom(ParseError::new(
                            pos,
                            "minmax() minimum cannot be flexible",
                        )));
                    }
                    Ok(TrackSize::MinMax(min, max))
                })
                .map_err(|_| ParseError::new(pos, "invalid minmax()"));
        }
        if lower == "fit-content" {
            return input
                .parse_nested_block(|input| {
                    let limit = parse_track_breadth(input, calcs)?;
                    input.expect_exhausted()?;
                    if !matches!(limit, TrackBreadth::Length(..) | TrackBreadth::Calc(_)) {
                        return Err(cssparser::ParseError::custom(ParseError::new(
                            pos,
                            "fit-content() takes a length or percentage",
                        )));
                    }
                    Ok(TrackSize::FitContent(limit))
                })
                .map_err(|_| ParseError::new(pos, "invalid fit-content()"));
        }
        if !super::calc::is_math_function(&lower) {
            return Err(ParseError::new(
                pos,
                format!("unsupported grid track function '{name}'"),
            ));
        }
    }
    input.reset(&probe);
    Ok(match breadth(input, calcs)? {
        TrackBreadth::Length(value, unit) => TrackSize::Length(value, unit),
        TrackBreadth::Calc(i) => TrackSize::Calc(i),
        TrackBreadth::Fraction(weight) => TrackSize::Fraction(weight),
        TrackBreadth::Auto => TrackSize::Auto,
        TrackBreadth::MinContent => TrackSize::MinContent,
        TrackBreadth::MaxContent => TrackSize::MaxContent,
    })
}

fn parse_track_breadth<'i>(
    input: &mut CssParser<'i>,
    calcs: &mut Vec<Calc>,
) -> Result<TrackBreadth, cssparser::ParseError<ParseError>> {
    breadth(input, calcs).map_err(cssparser::ParseError::custom)
}

/// One `<track-breadth>`; a math function is a length (folded) or a
/// `Calc` kept in `calcs` (an equal one is shared).
fn breadth<'i>(
    input: &mut CssParser<'i>,
    calcs: &mut Vec<Calc>,
) -> Result<TrackBreadth, ParseError> {
    input.skip_whitespace();
    let pos = input.position().byte_index();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(pos, err))?
        .clone();
    if let Token::Function(name) = &token
        && super::calc::is_math_function(name)
    {
        let lower = name.to_ascii_lowercase();
        let value = input
            .parse_nested_block(|inner| {
                super::calc::parse_math_function(&lower, inner).ok_or_else(|| {
                    cssparser::ParseError::<ParseError>::custom(ParseError::new(pos, ""))
                })
            })
            .map_err(|_| ParseError::new(pos, "invalid math function in a track"))?;
        return match value {
            Value::Length(v, unit) => Ok(TrackBreadth::Length(v, unit)),
            Value::Number(0.0) => Ok(TrackBreadth::Length(0.0, Unit::Px)),
            Value::Calc(calc) => {
                let index = match calcs.iter().position(|c| *c == *calc) {
                    Some(i) => i,
                    None => {
                        calcs.push(*calc);
                        calcs.len() - 1
                    }
                };
                u16::try_from(index)
                    .map(TrackBreadth::Calc)
                    .map_err(|_| ParseError::new(pos, "too many calc() tracks"))
            }
            _ => Err(ParseError::new(pos, "a track's calc() must be a length")),
        };
    }
    breadth_from_token(&token, pos)
}

fn breadth_from_token(token: &Token<'_>, pos: usize) -> Result<TrackBreadth, ParseError> {
    match token {
        Token::Ident(name) => match name.to_ascii_lowercase().as_str() {
            "auto" => Ok(TrackBreadth::Auto),
            "min-content" => Ok(TrackBreadth::MinContent),
            "max-content" => Ok(TrackBreadth::MaxContent),
            other => Err(ParseError::new(
                pos,
                format!("unsupported grid track keyword '{other}'"),
            )),
        },
        Token::Percentage { unit_value, .. } => {
            Ok(TrackBreadth::Length(unit_value * 100.0, Unit::Percent))
        }
        Token::Dimension { value, unit, .. } => {
            if unit.eq_ignore_ascii_case("fr") {
                return if *value >= 0.0 {
                    Ok(TrackBreadth::Fraction(*value))
                } else {
                    Err(ParseError::new(pos, "negative fr"))
                };
            }
            match super::calc::length_unit(unit) {
                Some(unit) => Ok(TrackBreadth::Length(*value, unit)),
                None => Err(ParseError::new(
                    pos,
                    format!("unsupported grid track unit '{unit}'"),
                )),
            }
        }
        // A unitless zero is a length (`minmax(0, 1fr)`); any other bare
        // number is not.
        Token::Number { value, .. } if *value == 0.0 => Ok(TrackBreadth::Length(0.0, Unit::Px)),
        Token::Number { .. } => Err(ParseError::new(
            pos,
            "grid track size requires a unit (px/em/rem/% or fr)",
        )),
        _ => Err(ParseError::new(pos, "invalid grid track size")),
    }
}
