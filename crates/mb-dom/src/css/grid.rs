// CSS grid value parsing — the longhand parsers for `grid-template-areas`,
// `grid-template-columns/rows` (track lists), and the per-item placement
// shorthand (`grid-column` / `grid-row`). The cascade routes these from
// `parse_declaration_value`; `length_with_unit` / `parse_function`
// helpers stay in mod.rs because they're cross-cutting.

use cssparser::{Parser as CssParser, Token};

use super::{
    GridLine, GridPlacement, ParseError, TrackSize, Unit, Value, convert_basic_error_at,
    token_error,
};

pub(super) fn parse_grid_template_areas<'i, 't>(
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

pub(super) fn parse_grid_placement<'i, 't>(
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

pub(super) fn parse_grid_track_list<'i, 't>(
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
            "ch" => Ok(TrackSize::Length(value, Unit::Ch)),
            "pt" => Ok(TrackSize::Length(value, Unit::Pt)),
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
