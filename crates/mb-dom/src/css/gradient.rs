// `linear-gradient(...)` / `radial-gradient(...)` value parsers. Both feed
// into the same `Value::Gradient(Gradient { kind, stops })` shape — only
// the prefix differs (linear takes an optional `to <side>` direction,
// radial currently ignores its positional/sizing arguments). The painter
// reads the resolved stops out of `Gradient::stops`.

use cssparser::{Parser as CssParser, Token};

use super::error::{convert_basic_error_at, token_error};
use super::parse::parse_value;
use super::{ColorStop, Gradient, GradientDirection, GradientKind, ParseError, Value};

pub(super) fn parse_linear_gradient<'i, 't>(
    input: &mut CssParser<'i, 't>,
) -> Result<Value, ParseError> {
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

pub(super) fn parse_radial_gradient<'i, 't>(
    input: &mut CssParser<'i, 't>,
) -> Result<Value, ParseError> {
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
