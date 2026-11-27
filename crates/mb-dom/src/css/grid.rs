// CSS grid value parsing — the longhand parsers for `grid-template-areas`,
// `grid-template-columns/rows` (track lists), and the per-item placement
// shorthand (`grid-column` / `grid-row`). The cascade routes these from
// `parse_declaration_value`; `length_with_unit` / `parse_function`
// helpers stay in mod.rs because they're cross-cutting.

use cssparser::{Parser as CssParser, Token};

use super::error::{convert_basic_error_at, token_error};
use super::{
    Declaration, GridLine, GridPlacement, ParseError, TrackBreadth, TrackSize, Unit, Value,
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
/// Line names (`[a]`) and `repeat()` are not read — the declaration is
/// dropped, as before.
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
        let areas = parse_area_rows(input, |input| {
            input.skip_whitespace();
            let probe = input.state();
            let size = match input.next() {
                Ok(Token::QuotedString(_)) | Ok(Token::Delim('/')) | Err(_) => None,
                Ok(_) => {
                    input.reset(&probe);
                    Some(parse_grid_track_size(input)?)
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
            if columns.is_empty() {
                return Err(ParseError::new(
                    input.position().byte_index(),
                    "grid-template needs columns after '/'",
                ));
            }
            Value::TrackList(columns)
        } else {
            none()
        };
        input
            .expect_exhausted()
            .map_err(|err| convert_basic_error_at(input.position().byte_index(), err))?;
        return Ok(declarations(
            Value::TrackList(row_sizes),
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
    if tracks.is_empty() {
        return Err(ParseError::new(
            input.position().byte_index(),
            "grid track list requires at least one track size",
        ));
    }
    Ok(Value::TrackList(tracks))
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

/// Track sizes up to the end of the input or a `/` (left unread).
fn parse_track_sizes<'i>(input: &mut CssParser<'i>) -> Result<Vec<TrackSize>, ParseError> {
    let mut tracks = Vec::new();
    loop {
        input.skip_whitespace();
        let probe = input.state();
        match input.next() {
            Err(_) => break,
            Ok(Token::Delim('/')) => {
                input.reset(&probe);
                break;
            }
            Ok(_) => input.reset(&probe),
        }
        tracks.push(parse_grid_track_size(input)?);
    }
    Ok(tracks)
}

/// `<track-size>`: a breadth (not `fr`-only rules: `1fr` is a track size
/// on its own) or `minmax(<min>, <max>)`. `fit-content()`, `repeat()`
/// and `calc()` stay unsupported (step 5 owns calc in tracks).
fn parse_grid_track_size<'i>(input: &mut CssParser<'i>) -> Result<TrackSize, ParseError> {
    let pos = input.position().byte_index();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(pos, err))?
        .clone();
    if let Token::Function(name) = &token {
        if !name.eq_ignore_ascii_case("minmax") {
            return Err(ParseError::new(
                pos,
                format!("unsupported grid track function '{name}'"),
            ));
        }
        return input
            .parse_nested_block(|input| {
                let min = parse_track_breadth(input)?;
                input.expect_comma()?;
                let max = parse_track_breadth(input)?;
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
    Ok(match breadth_from_token(&token, pos)? {
        TrackBreadth::Length(value, unit) => TrackSize::Length(value, unit),
        TrackBreadth::Fraction(weight) => TrackSize::Fraction(weight),
        TrackBreadth::Auto => TrackSize::Auto,
        TrackBreadth::MinContent => TrackSize::MinContent,
        TrackBreadth::MaxContent => TrackSize::MaxContent,
    })
}

fn parse_track_breadth<'i>(
    input: &mut CssParser<'i>,
) -> Result<TrackBreadth, cssparser::ParseError<ParseError>> {
    let pos = input.position().byte_index();
    let token = input.next()?.clone();
    breadth_from_token(&token, pos).map_err(cssparser::ParseError::custom)
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
        Token::Dimension { value, unit, .. } => match unit.to_ascii_lowercase().as_str() {
            "fr" if *value >= 0.0 => Ok(TrackBreadth::Fraction(*value)),
            "px" => Ok(TrackBreadth::Length(*value, Unit::Px)),
            "em" => Ok(TrackBreadth::Length(*value, Unit::Em)),
            "rem" => Ok(TrackBreadth::Length(*value, Unit::Rem)),
            "ch" => Ok(TrackBreadth::Length(*value, Unit::Ch)),
            "pt" => Ok(TrackBreadth::Length(*value, Unit::Pt)),
            other => Err(ParseError::new(
                pos,
                format!("unsupported grid track unit '{other}'"),
            )),
        },
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
