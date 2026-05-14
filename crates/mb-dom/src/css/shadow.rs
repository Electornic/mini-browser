// `box-shadow` and `text-shadow` value parsers. Grammar:
//
//     box-shadow:  <offset-x> <offset-y> [blur [spread]]? [<color>]?
//     text-shadow: <offset-x> <offset-y> [blur]?            [<color>]?
//
// The MVP only supports the single-shadow form (no comma-separated stack)
// and ignores the `inset` keyword on box-shadow; both are enough for the
// pages the renderer targets today. A missing color falls back to opaque
// black, matching how the painter resolves `currentColor`.

use cssparser::Parser as CssParser;

use super::parse::{parse_length_token, parse_value, peek_starts_length};
use super::{BoxShadow, Color, ParseError, TextShadow, Value};

pub(super) fn parse_box_shadow_value<'i, 't>(
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

pub(super) fn parse_text_shadow_value<'i, 't>(
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
