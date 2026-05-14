// Generic CSS value parsing. `parse_value` is the fallback dispatch the
// per-property parsers fall through to when they don't have a custom
// shape (keywords, numbers, hex/named colors, the `rgb()` / `url()` /
// `var()` functions). Length / number tokenisers used by the shorthand
// and transform/shadow parsers live here too, alongside `length_with_unit`
// which maps a CSS dimension to a `Value::Length(_, Unit)`.

use cssparser::{Parser as CssParser, Token};

use super::error::{convert_basic_error_at, convert_error, token_error};
use super::gradient::{parse_linear_gradient, parse_radial_gradient};
use super::color::named_color;
use super::{Color, ParseError, Unit, Value};

pub(super) fn parse_value<'i, 't>(input: &mut CssParser<'i, 't>) -> Result<Value, ParseError> {
    input.skip_whitespace();
    let position = input.position();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(position.byte_index(), err))?
        .clone();

    let value = match token {
        Token::Hash(hex) | Token::IDHash(hex) => parse_hex_color_str(hex.as_ref(), input)?,
        Token::Number { value, .. } => Value::Number(value),
        Token::Percentage { unit_value, .. } => Value::Length(unit_value * 100.0, Unit::Percent),
        Token::Dimension { value, unit, .. } => length_with_unit(value, unit.as_ref()),
        Token::Ident(ident) => {
            let raw = ident.to_string();
            if let Some(color) = named_color(&raw) {
                Value::Color(color)
            } else {
                Value::Keyword(raw)
            }
        }
        Token::Function(name) => parse_function(name.as_ref(), input)?,
        // Unquoted URL: cssparser turns the entire `url(  /a/b  )` into a single
        // `UnquotedUrl` token (already trimmed of inner whitespace), so the
        // outer `Function("url")` route only fires on the quoted form.
        Token::UnquotedUrl(url) => Value::ImageUrl(url.to_string()),
        Token::QuotedString(s) => Value::Keyword(s.to_string()),
        other => {
            return Err(token_error(input, &other, "unexpected token in value"));
        }
    };

    Ok(value)
}

pub(super) fn length_with_unit(value: f32, unit: &str) -> Value {
    match unit {
        "px" => Value::Length(value, Unit::Px),
        "em" => Value::Length(value, Unit::Em),
        "rem" => Value::Length(value, Unit::Rem),
        "ch" => Value::Length(value, Unit::Ch),
        "pt" => Value::Length(value, Unit::Pt),
        // Unsupported dimensions fall back to a keyword that mirrors the original
        // tokens so callers can still distinguish them at the cascade layer.
        other => Value::Keyword(format!("{value}{other}")),
    }
}

fn parse_hex_color_str<'i, 't>(
    hex: &str,
    input: &CssParser<'i, 't>,
) -> Result<Value, ParseError> {
    let (r, g, b) = match hex.len() {
        3 => {
            let chars: Vec<char> = hex.chars().collect();
            (
                expand_hex(chars[0])?,
                expand_hex(chars[1])?,
                expand_hex(chars[2])?,
            )
        }
        6 => (
            parse_hex_pair(&hex[0..2])?,
            parse_hex_pair(&hex[2..4])?,
            parse_hex_pair(&hex[4..6])?,
        ),
        _ => {
            return Err(ParseError::new(
                input.position().byte_index(),
                "hex colors must use either 3 or 6 digits",
            ));
        }
    };
    Ok(Value::Color(Color { r, g, b, a: 255 }))
}

fn parse_hex_pair(pair: &str) -> Result<u8, ParseError> {
    u8::from_str_radix(pair, 16)
        .map_err(|_| ParseError::new(0, format!("invalid hex color pair '{pair}'")))
}

fn expand_hex(ch: char) -> Result<u8, ParseError> {
    let mut pair = String::with_capacity(2);
    pair.push(ch);
    pair.push(ch);
    parse_hex_pair(&pair)
}

/// Drive a function-call value (`rgb()`, `linear-gradient()`, etc). The caller
/// has already consumed the leading `Function(name)` token, so we open a nested
/// block and route by name.
fn parse_function<'i, 't>(
    name: &str,
    input: &mut CssParser<'i, 't>,
) -> Result<Value, ParseError> {
    let lower = name.to_ascii_lowercase();
    input
        .parse_nested_block(|inner| {
            let result: Result<Value, ParseError> = match lower.as_str() {
                "rgb" => parse_rgb_function(inner, false),
                "rgba" => parse_rgb_function(inner, true),
                "linear-gradient" => parse_linear_gradient(inner),
                "radial-gradient" => parse_radial_gradient(inner),
                "url" => parse_url_function(inner),
                "var" => parse_var_function(inner),
                other => Err(ParseError::new(
                    inner.position().byte_index(),
                    format!("unsupported function '{other}'"),
                )),
            };
            result.map_err(|err| inner.new_custom_error(err))
        })
        .map_err(|err| convert_error(err))
}

fn parse_rgb_function<'i, 't>(
    input: &mut CssParser<'i, 't>,
    has_alpha: bool,
) -> Result<Value, ParseError> {
    let r = parse_color_byte(input)?;
    expect_token_comma(input)?;
    let g = parse_color_byte(input)?;
    expect_token_comma(input)?;
    let b = parse_color_byte(input)?;
    let a = if has_alpha {
        expect_token_comma(input)?;
        let alpha = parse_unsigned_number(input)?;
        (alpha.clamp(0.0, 1.0) * 255.0).round() as u8
    } else {
        255
    };
    input.skip_whitespace();
    Ok(Value::Color(Color { r, g, b, a }))
}

fn parse_color_byte<'i, 't>(input: &mut CssParser<'i, 't>) -> Result<u8, ParseError> {
    let value = parse_unsigned_number(input)?;
    Ok(value.clamp(0.0, 255.0).round() as u8)
}

fn parse_unsigned_number<'i, 't>(input: &mut CssParser<'i, 't>) -> Result<f32, ParseError> {
    input.skip_whitespace();
    let pos = input.position().byte_index();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(pos, err))?
        .clone();
    match token {
        Token::Number { value, .. } => Ok(value),
        other => Err(token_error(input, &other, "invalid numeric component")),
    }
}

fn expect_token_comma<'i, 't>(input: &mut CssParser<'i, 't>) -> Result<(), ParseError> {
    input.skip_whitespace();
    let pos = input.position().byte_index();
    input
        .expect_comma()
        .map_err(|err| convert_basic_error_at(pos, err))
}

fn parse_url_function<'i, 't>(input: &mut CssParser<'i, 't>) -> Result<Value, ParseError> {
    // Inside the nested block opened by `Function("url")`. The token for the URL
    // is either an `UnquotedUrl` (cssparser already trimmed surrounding
    // whitespace) or a `QuotedString`. Anything else is a parse error.
    input.skip_whitespace();
    let pos = input.position().byte_index();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(pos, err))?
        .clone();
    let url = match token {
        Token::UnquotedUrl(url) => url.to_string(),
        Token::QuotedString(url) => url.to_string(),
        other => {
            return Err(token_error(input, &other, "expected URL token"));
        }
    };
    input.skip_whitespace();
    Ok(Value::ImageUrl(url))
}

/// Inside the nested block opened by `Function("var")`. cssparser tokenises
/// `--name` as a regular `Token::Ident` (CSS Syntax L3 allows leading `--`),
/// so the first token is always the property name. A trailing fallback after
/// the comma is parsed by reusing the generic `parse_value`, which means the
/// fallback inherits whatever value shapes that helper accepts (colors,
/// lengths, keywords, even a nested `var()`). The value returned here is
/// substituted later in `style::resolve_var` once cascade has gathered the
/// `--*` declarations in scope.
fn parse_var_function<'i, 't>(input: &mut CssParser<'i, 't>) -> Result<Value, ParseError> {
    input.skip_whitespace();
    let pos = input.position().byte_index();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(pos, err))?
        .clone();
    let name = match token {
        Token::Ident(ident) if ident.starts_with("--") => ident.to_string(),
        other => {
            return Err(token_error(
                input,
                &other,
                "var() expects a custom-property name starting with '--'",
            ));
        }
    };
    input.skip_whitespace();

    // Optional fallback after a comma. `try_parse` only commits on success so
    // we either consume the comma + parse the fallback, or leave the parser
    // positioned at the closing `)`.
    let fallback = if input
        .try_parse(|p| p.expect_comma())
        .is_ok()
    {
        Some(Box::new(parse_value(input)?))
    } else {
        None
    };
    input.skip_whitespace();

    Ok(Value::Var { name, fallback })
}

/// Parse a single number-or-length token where a leading sign is allowed. Used
/// by transform / shadow / flex / border-radius — anywhere we want to accept
/// `-10px` or `1.5` without the generic `parse_value` machinery.
pub(super) fn parse_length_or_number<'i, 't>(
    input: &mut CssParser<'i, 't>,
) -> Result<Value, ParseError> {
    input.skip_whitespace();
    let pos = input.position().byte_index();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(pos, err))?
        .clone();
    match token {
        Token::Number { value, .. } => Ok(Value::Number(value)),
        Token::Percentage { unit_value, .. } => Ok(Value::Length(unit_value * 100.0, Unit::Percent)),
        Token::Dimension { value, unit, .. } => Ok(length_with_unit(value, unit.as_ref())),
        other => Err(token_error(input, &other, "expected a length or number")),
    }
}

pub(super) fn parse_length_token<'i, 't>(input: &mut CssParser<'i, 't>) -> Result<f32, ParseError> {
    match parse_length_or_number(input)? {
        Value::Length(v, _) => Ok(v),
        Value::Number(v) => Ok(v),
        other => Err(ParseError::new(
            input.position().byte_index(),
            format!("expected a length token, got {other:?}"),
        )),
    }
}

/// `true` if the next token can begin a numeric value (number, dimension,
/// percentage). Used by shadow / flex parsers that greedily consume a variable
/// number of leading lengths.
pub(super) fn peek_starts_length<'i, 't>(input: &mut CssParser<'i, 't>) -> bool {
    let saved = input.state();
    let result = matches!(
        input.next(),
        Ok(Token::Number { .. }) | Ok(Token::Dimension { .. }) | Ok(Token::Percentage { .. })
    );
    input.reset(&saved);
    result
}
