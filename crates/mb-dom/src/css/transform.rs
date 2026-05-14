// `transform: <function-list>` value parser. We accept a space-separated
// list of `translate / translateX / translateY / scale / scaleX / scaleY
// / rotate` calls and emit a `Value::TransformList(Vec<TransformOp>)`
// that the layout/paint pipelines compose into a single affine.

use cssparser::{Parser as CssParser, Token};

use super::error::{convert_basic_error_at, convert_error, token_error};
use super::parse::parse_length_token;
use super::{ParseError, TransformOp, Value};

pub(super) fn parse_transform_value<'i, 't>(
    input: &mut CssParser<'i, 't>,
) -> Result<Value, ParseError> {
    let mut ops = Vec::new();
    loop {
        input.skip_whitespace();
        // Stop at end-of-input (we're in a delimited declaration slice, so
        // `next()` returning Err means there's nothing more).
        let probe = input.state();
        let func = match input.next() {
            Ok(Token::Function(name)) => name.to_string(),
            Ok(_) => {
                input.reset(&probe);
                break;
            }
            Err(_) => {
                input.reset(&probe);
                break;
            }
        };
        let op = input
            .parse_nested_block(|inner| {
                let result: Result<TransformOp, ParseError> = parse_transform_op(&func, inner);
                result.map_err(|err| inner.new_custom_error(err))
            })
            .map_err(|err| convert_error(err))?;
        ops.push(op);
    }
    if ops.is_empty() {
        return Err(ParseError::new(
            input.position().byte_index(),
            "transform requires at least one function",
        ));
    }
    Ok(Value::TransformList(ops))
}

fn parse_transform_op<'i, 't>(
    name: &str,
    input: &mut CssParser<'i, 't>,
) -> Result<TransformOp, ParseError> {
    match name {
        "translate" => {
            input.skip_whitespace();
            let x = parse_length_token(input)?;
            input.skip_whitespace();
            let y = if input.try_parse(|i| i.expect_comma()).is_ok() {
                input.skip_whitespace();
                let value = parse_length_token(input)?;
                input.skip_whitespace();
                value
            } else {
                0.0
            };
            Ok(TransformOp::Translate { x, y })
        }
        "translateX" => {
            input.skip_whitespace();
            let x = parse_length_token(input)?;
            input.skip_whitespace();
            Ok(TransformOp::Translate { x, y: 0.0 })
        }
        "translateY" => {
            input.skip_whitespace();
            let y = parse_length_token(input)?;
            input.skip_whitespace();
            Ok(TransformOp::Translate { x: 0.0, y })
        }
        "scale" => {
            input.skip_whitespace();
            let x = parse_length_token(input)?;
            input.skip_whitespace();
            let y = if input.try_parse(|i| i.expect_comma()).is_ok() {
                input.skip_whitespace();
                let value = parse_length_token(input)?;
                input.skip_whitespace();
                value
            } else {
                x
            };
            Ok(TransformOp::Scale { x, y })
        }
        "scaleX" => {
            input.skip_whitespace();
            let x = parse_length_token(input)?;
            input.skip_whitespace();
            Ok(TransformOp::Scale { x, y: 1.0 })
        }
        "scaleY" => {
            input.skip_whitespace();
            let y = parse_length_token(input)?;
            input.skip_whitespace();
            Ok(TransformOp::Scale { x: 1.0, y })
        }
        "rotate" => {
            input.skip_whitespace();
            let theta = parse_angle_token(input)?;
            input.skip_whitespace();
            Ok(TransformOp::Rotate(theta))
        }
        other => Err(ParseError::new(
            input.position().byte_index(),
            format!("unsupported transform function '{other}'"),
        )),
    }
}

fn parse_angle_token<'i, 't>(input: &mut CssParser<'i, 't>) -> Result<f32, ParseError> {
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
