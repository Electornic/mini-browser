// `box-shadow` and `text-shadow` value parsers. Grammar:
//
//     box-shadow:  <offset-x> <offset-y> [blur [spread]]? [<color>]?
//     text-shadow: <offset-x> <offset-y> [blur]?            [<color>]?
//
// The MVP only supports the single-shadow form (no comma-separated stack)
// and ignores the `inset` keyword on box-shadow; both are enough for the
// pages the renderer targets today. A missing color falls back to opaque
// black, matching how the painter resolves `currentColor`.

use cssparser::{Delimiter, Parser as CssParser};

use super::error::{convert_basic_error_at, convert_error};
use super::parse::{parse_length_token, parse_value, peek_starts_length};
use super::{BoxShadow, Color, ParseError, TextShadow, Value};

/// `box-shadow`: a comma list of layers, each `inset? && <x> <y> [blur
/// [spread]]? && <color>?` (colour before or after the lengths). Every
/// layer must parse; the first **outset** layer is kept and painted (one
/// shadow is modelled) — an all-inset list paints nothing (`none`), rather
/// than drawing an inset shadow outside the box (step 1b).
pub(super) fn parse_box_shadow_value<'i>(input: &mut CssParser<'i>) -> Result<Value, ParseError> {
    if input
        .try_parse(|i| {
            i.expect_ident_matching("none")?;
            i.expect_exhausted()
        })
        .is_ok()
    {
        return Ok(Value::Keyword("none".into()));
    }
    let mut first_outset: Option<BoxShadow> = None;
    loop {
        let (inset, shadow) = input
            .parse_until_before(Delimiter::Comma, |layer| {
                parse_box_shadow_layer(layer).map_err(cssparser::ParseError::custom)
            })
            .map_err(convert_error)?;
        if !inset && first_outset.is_none() {
            first_outset = Some(shadow);
        }
        if input.is_exhausted() {
            break;
        }
        input
            .expect_comma()
            .map_err(|e| convert_basic_error_at(input.position().byte_index(), e))?;
    }
    Ok(first_outset.map_or(Value::Keyword("none".into()), Value::BoxShadow))
}

fn parse_box_shadow_layer<'i>(input: &mut CssParser<'i>) -> Result<(bool, BoxShadow), ParseError> {
    let mut inset = false;
    let mut color: Option<Color> = None;
    let mut lengths: Vec<f32> = Vec::new();
    loop {
        input.skip_whitespace();
        if input.is_exhausted() {
            break;
        }
        if !inset
            && input
                .try_parse(|i| i.expect_ident_matching("inset"))
                .is_ok()
        {
            inset = true;
            continue;
        }
        if lengths.is_empty() && peek_starts_length(input) {
            while lengths.len() < 4 && peek_starts_length(input) {
                lengths.push(parse_length_token(input)?);
                input.skip_whitespace();
            }
            continue;
        }
        if color.is_none() {
            match parse_value(input)? {
                Value::Color(c) => {
                    color = Some(c);
                    continue;
                }
                other => {
                    return Err(ParseError::new(
                        input.position().byte_index(),
                        format!("expected color in box-shadow, got {other:?}"),
                    ));
                }
            }
        }
        return Err(ParseError::new(
            input.position().byte_index(),
            "box-shadow: unexpected value",
        ));
    }
    let [offset_x, offset_y, blur, spread] = match lengths.as_slice() {
        [x, y] => [*x, *y, 0.0, 0.0],
        [x, y, b] => [*x, *y, *b, 0.0],
        [x, y, b, s] => [*x, *y, *b, *s],
        _ => {
            return Err(ParseError::new(
                input.position().byte_index(),
                "box-shadow: needs 2–4 lengths",
            ));
        }
    };
    Ok((
        inset,
        BoxShadow {
            offset_x,
            offset_y,
            blur_radius: blur.max(0.0),
            spread_radius: spread,
            color: color.unwrap_or(Color {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            }),
        },
    ))
}

pub(super) fn parse_text_shadow_value<'i>(input: &mut CssParser<'i>) -> Result<Value, ParseError> {
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
