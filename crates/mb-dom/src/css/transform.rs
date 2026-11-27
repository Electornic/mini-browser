// `transform: none | <function-list>` value parser. Functions: translate
// (and -X/-Y/-Z/3d), scale (and -X/-Y/3d), rotate / rotateZ, matrix. The z
// parts are dropped — the engine paints in 2D with no perspective, where
// they change nothing. Emits a `Value::TransformList(Vec<TransformOp>)`
// that paint composes into a single affine around the box centre.

use cssparser::{Parser as CssParser, Token};

use super::error::{convert_basic_error_at, convert_error, token_error};
use super::{ParseError, TransformOp, Value};

pub(super) fn parse_transform_value<'i>(input: &mut CssParser<'i>) -> Result<Value, ParseError> {
    // `none` alone (follow-up F2): no transform — overrides an earlier one.
    if input.try_parse(|i| i.expect_ident_matching("none")).is_ok() {
        return Ok(Value::TransformList(Vec::new()));
    }
    let mut ops = Vec::new();
    let mut functions = 0;
    loop {
        input.skip_whitespace();
        // Stop at end-of-input (we're in a delimited declaration slice, so
        // `next()` returning Err means there's nothing more); anything else
        // is left for `parse_entirely` to reject.
        let probe = input.state();
        let func = match input.next() {
            Ok(Token::Function(name)) => name.to_ascii_lowercase(),
            _ => {
                input.reset(&probe);
                break;
            }
        };
        let parsed = input
            .parse_nested_block(|inner| {
                let result: Result<Vec<TransformOp>, ParseError> = parse_transform_op(&func, inner);
                result.map_err(cssparser::ParseError::custom)
            })
            .map_err(convert_error)?;
        ops.extend(parsed);
        functions += 1;
    }
    if functions == 0 {
        return Err(ParseError::new(
            input.position().byte_index(),
            "transform requires `none` or at least one function",
        ));
    }
    Ok(Value::TransformList(ops))
}

/// A translate argument: px, or a percentage of the box's own border box.
#[derive(Clone, Copy)]
enum Offset {
    Px(f32),
    Percent(f32),
}

/// `<length-percentage>` limited to what paint can place: px, `%`, and a
/// unitless 0. Other units (`em`, `vw` …) are invalid here rather than read
/// as px (they used to be, silently).
fn parse_offset<'i>(input: &mut CssParser<'i>) -> Result<Offset, ParseError> {
    input.skip_whitespace();
    let pos = input.position().byte_index();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(pos, err))?
        .clone();
    let offset = match token {
        Token::Dimension {
            value, ref unit, ..
        } if unit.eq_ignore_ascii_case("px") => Offset::Px(value),
        Token::Number { value: 0.0, .. } => Offset::Px(0.0),
        Token::Percentage { unit_value, .. } => Offset::Percent(unit_value * 100.0),
        other => return Err(token_error(input, &other, "invalid translate offset")),
    };
    input.skip_whitespace();
    Ok(offset)
}

/// `<number> | <percentage>` (a scale factor).
fn parse_factor<'i>(input: &mut CssParser<'i>) -> Result<f32, ParseError> {
    input.skip_whitespace();
    let pos = input.position().byte_index();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(pos, err))?
        .clone();
    let factor = match token {
        Token::Number { value, .. } => value,
        Token::Percentage { unit_value, .. } => unit_value,
        other => return Err(token_error(input, &other, "invalid scale factor")),
    };
    input.skip_whitespace();
    Ok(factor)
}

fn parse_number<'i>(input: &mut CssParser<'i>) -> Result<f32, ParseError> {
    input.skip_whitespace();
    let pos = input.position().byte_index();
    let value = input
        .expect_number()
        .map_err(|err| convert_basic_error_at(pos, err))?;
    input.skip_whitespace();
    Ok(value)
}

fn comma<'i>(input: &mut CssParser<'i>) -> Result<(), ParseError> {
    let pos = input.position().byte_index();
    input
        .expect_comma()
        .map_err(|err| convert_basic_error_at(pos, err))
}

/// `translate(x, y)` as ops: px and percentage parts separately (they add).
fn translate(x: Offset, y: Offset) -> Vec<TransformOp> {
    let (mut px, mut pct) = ((0.0, 0.0), (0.0, 0.0));
    match x {
        Offset::Px(v) => px.0 = v,
        Offset::Percent(v) => pct.0 = v,
    }
    match y {
        Offset::Px(v) => px.1 = v,
        Offset::Percent(v) => pct.1 = v,
    }
    let mut ops = Vec::new();
    if pct != (0.0, 0.0) {
        ops.push(TransformOp::TranslatePercent { x: pct.0, y: pct.1 });
    }
    if px != (0.0, 0.0) || ops.is_empty() {
        ops.push(TransformOp::Translate { x: px.0, y: px.1 });
    }
    ops
}

/// One function's ops (none for a pure z translation).
fn parse_transform_op<'i>(
    name: &str,
    input: &mut CssParser<'i>,
) -> Result<Vec<TransformOp>, ParseError> {
    let ops = match name {
        "translate" => {
            let x = parse_offset(input)?;
            let y = if input.try_parse(|i| i.expect_comma()).is_ok() {
                parse_offset(input)?
            } else {
                Offset::Px(0.0)
            };
            translate(x, y)
        }
        "translatex" => translate(parse_offset(input)?, Offset::Px(0.0)),
        "translatey" => translate(Offset::Px(0.0), parse_offset(input)?),
        "translatez" => {
            // A length (no percentage); no 2D effect.
            if matches!(parse_offset(input)?, Offset::Percent(_)) {
                return Err(ParseError::new(0, "translateZ takes a length"));
            }
            Vec::new()
        }
        "translate3d" => {
            let x = parse_offset(input)?;
            comma(input)?;
            let y = parse_offset(input)?;
            comma(input)?;
            if matches!(parse_offset(input)?, Offset::Percent(_)) {
                return Err(ParseError::new(0, "translate3d's z takes a length"));
            }
            translate(x, y)
        }
        "scale" => {
            let x = parse_factor(input)?;
            let y = if input.try_parse(|i| i.expect_comma()).is_ok() {
                parse_factor(input)?
            } else {
                x
            };
            vec![TransformOp::Scale { x, y }]
        }
        "scalex" => vec![TransformOp::Scale {
            x: parse_factor(input)?,
            y: 1.0,
        }],
        "scaley" => vec![TransformOp::Scale {
            x: 1.0,
            y: parse_factor(input)?,
        }],
        "scale3d" => {
            let x = parse_factor(input)?;
            comma(input)?;
            let y = parse_factor(input)?;
            comma(input)?;
            parse_factor(input)?;
            vec![TransformOp::Scale { x, y }]
        }
        "rotate" | "rotatez" => {
            input.skip_whitespace();
            let theta = parse_angle_token(input)?;
            input.skip_whitespace();
            vec![TransformOp::Rotate(theta)]
        }
        "matrix" => {
            let mut m = [0.0; 6];
            for (i, slot) in m.iter_mut().enumerate() {
                if i > 0 {
                    comma(input)?;
                }
                *slot = parse_number(input)?;
            }
            vec![TransformOp::Matrix(m)]
        }
        other => {
            return Err(ParseError::new(
                input.position().byte_index(),
                format!("unsupported transform function '{other}'"),
            ));
        }
    };
    Ok(ops)
}

fn parse_angle_token<'i>(input: &mut CssParser<'i>) -> Result<f32, ParseError> {
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
