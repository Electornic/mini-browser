// Generic CSS value parsing. `parse_value` is the fallback dispatch the
// per-property parsers fall through to when they don't have a custom
// shape (keywords, numbers, hex/named colors, the `rgb()` / `url()` /
// `var()` functions). Length / number tokenisers used by the shorthand
// and transform/shadow parsers live here too, alongside `length_with_unit`
// which maps a CSS dimension to a `Value::Length(_, Unit)`.

use cssparser::{Parser as CssParser, Token};

use super::color::named_color;
use super::error::{convert_basic_error_at, convert_error, token_error};
use super::gradient::{parse_linear_gradient, parse_radial_gradient};
use super::{ClipBox, ClipEdge, ClipFunction, Color, ParseError, Unit, Value};

pub(super) fn parse_value<'i>(input: &mut CssParser<'i>) -> Result<Value, ParseError> {
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
            } else if raw.eq_ignore_ascii_case("currentcolor") {
                // One spelling for the style pass to resolve (F2).
                Value::Keyword("currentcolor".into())
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

fn parse_hex_color_str<'i>(hex: &str, input: &CssParser<'i>) -> Result<Value, ParseError> {
    // A hash token's value is an identifier and may be multibyte; the
    // length match and byte slicing below assume ASCII.
    let len = if hex.is_ascii() { hex.len() } else { 0 };
    let digit = |i: usize| -> Result<u8, ParseError> {
        let c = hex.as_bytes().get(i).copied().unwrap_or(b'x') as char;
        expand_hex(c)
    };
    let pair = |i: usize| -> Result<u8, ParseError> { parse_hex_pair(&hex[i..i + 2]) };
    // #rgb, #rgba (CSS Color 4 — follow-up F2), #rrggbb, #rrggbbaa.
    let (r, g, b, a) = match len {
        3 => (digit(0)?, digit(1)?, digit(2)?, 255),
        4 => (digit(0)?, digit(1)?, digit(2)?, digit(3)?),
        6 => (pair(0)?, pair(2)?, pair(4)?, 255),
        8 => (pair(0)?, pair(2)?, pair(4)?, pair(6)?),
        _ => {
            return Err(ParseError::new(
                input.position().byte_index(),
                "hex colors use 3, 4, 6 or 8 digits",
            ));
        }
    };
    Ok(Value::Color(Color { r, g, b, a }))
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

thread_local! {
    /// How many `parse_function` calls are open on this thread: value
    /// functions nest through `parse_value` (`light-dark(light-dark(…))`,
    /// gradient stops), each a recursion.
    static FUNCTION_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Drive a function-call value (`rgb()`, `linear-gradient()`, etc). The caller
/// has already consumed the leading `Function(name)` token, so we open a nested
/// block and route by name.
fn parse_function<'i>(name: &str, input: &mut CssParser<'i>) -> Result<Value, ParseError> {
    // Bounded like the `var()` walks (Robustness Rule 2 — an overflow is an
    // abort): past `MAX_VAR_NESTING` the value is invalid.
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            FUNCTION_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        }
    }
    let depth = FUNCTION_DEPTH.with(|d| {
        d.set(d.get() + 1);
        d.get()
    });
    let _leave = Leave;
    if depth > super::MAX_VAR_NESTING {
        return Err(ParseError::new(
            input.position().byte_index(),
            "functions nested too deeply",
        ));
    }
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
                "light-dark" => parse_light_dark(inner),
                other => Err(ParseError::new(
                    inner.position().byte_index(),
                    format!("unsupported function '{other}'"),
                )),
            };
            result.map_err(cssparser::ParseError::custom)
        })
        .map_err(convert_error)
}

/// `light-dark(<light>, <dark>)` → the light value: the engine renders the
/// light scheme only (step 1b). Both arguments must parse.
fn parse_light_dark<'i>(input: &mut CssParser<'i>) -> Result<Value, ParseError> {
    let light = parse_value(input)?;
    expect_token_comma(input)?;
    parse_value(input)?;
    input.skip_whitespace();
    input
        .expect_exhausted()
        .map_err(|e| convert_basic_error_at(input.position().byte_index(), e))?;
    Ok(light)
}

/// `font-family`: a comma list of family names — a quoted string, or one
/// or more identifiers joined by single spaces (`Apple SD Gothic Neo`).
/// Generic families (`monospace`) are names like any other here.
pub(super) fn parse_font_family<'i>(input: &mut CssParser<'i>) -> Result<Value, ParseError> {
    let mut families = Vec::new();
    loop {
        input.skip_whitespace();
        let position = input.position().byte_index();
        let mut words: Vec<String> = Vec::new();
        loop {
            input.skip_whitespace();
            let state = input.state();
            match input.next() {
                Ok(Token::QuotedString(s)) if words.is_empty() => {
                    words.push(s.to_string());
                    break;
                }
                Ok(Token::Ident(ident)) => words.push(ident.to_string()),
                _ => {
                    input.reset(&state);
                    break;
                }
            }
        }
        if words.is_empty() {
            return Err(ParseError::new(
                position,
                "font-family: expected a family name",
            ));
        }
        families.push(words.join(" "));
        input.skip_whitespace();
        if input.is_exhausted() {
            return Ok(Value::FontFamilies(families));
        }
        expect_token_comma(input)?;
    }
}

/// Properties the engine accepts and keeps but nothing downstream reads
/// (step 1b "parse-only": `transition*`, `animation*` and their `-webkit-`
/// forms): any token sequence, stored as its source text, so they stop
/// counting as dropped declarations.
pub(super) fn is_parse_only(name: &str) -> bool {
    let name = name.strip_prefix("-webkit-").unwrap_or(name);
    name.starts_with("transition") || name.starts_with("animation")
}

pub(super) fn parse_raw<'i>(input: &mut CssParser<'i>) -> Result<Value, ParseError> {
    input.skip_whitespace();
    let start = input.position();
    while input.next().is_ok() {}
    let raw = input.slice_from(start).trim().to_string();
    if raw.is_empty() {
        return Err(ParseError::new(start.byte_index(), "empty value"));
    }
    Ok(Value::Keyword(raw))
}

/// `clip: rect(<top>, <right>, <bottom>, <left>)` (commas or spaces;
/// lengths or `auto`) and `clip-path: inset(<1–4 lengths> [round …]?)`
/// (step 1b — kept for step 3's clipping). Anything else goes through the
/// generic single-token parser (`auto`, `none`).
pub(super) fn parse_clip<'i>(name: &str, input: &mut CssParser<'i>) -> Result<Value, ParseError> {
    let function = if name == "clip" { "rect" } else { "inset" };
    let state = input.state();
    let is_function =
        matches!(input.next(), Ok(Token::Function(f)) if f.eq_ignore_ascii_case(function));
    if !is_function {
        input.reset(&state);
        return parse_value(input);
    }
    let edges = input
        .parse_nested_block(|inner| {
            parse_clip_edges(function, inner).map_err(cssparser::ParseError::custom)
        })
        .map_err(convert_error)?;
    Ok(Value::ClipBox(ClipBox {
        function: if function == "rect" {
            ClipFunction::Rect
        } else {
            ClipFunction::Inset
        },
        edges,
    }))
}

fn parse_clip_edges<'i>(
    function: &str,
    input: &mut CssParser<'i>,
) -> Result<[ClipEdge; 4], ParseError> {
    let mut edges = Vec::new();
    loop {
        input.skip_whitespace();
        if function == "inset"
            && input
                .try_parse(|i| i.expect_ident_matching("round"))
                .is_ok()
        {
            // Corner radii: parsed, not kept (rounded clipping is out of scope).
            while input.next().is_ok() {}
            break;
        }
        if input.is_exhausted() || edges.len() == 4 {
            break;
        }
        if function == "rect" && !edges.is_empty() {
            let _ = input.try_parse(|i| i.expect_comma());
            input.skip_whitespace();
        }
        if function == "rect" && input.try_parse(|i| i.expect_ident_matching("auto")).is_ok() {
            edges.push(ClipEdge::Auto);
            continue;
        }
        edges.push(match parse_length_or_number(input)? {
            Value::Number(0.0) => ClipEdge::Length(0.0, Unit::Px),
            Value::Length(v, unit) => ClipEdge::Length(v, unit),
            _ => {
                return Err(ParseError::new(
                    input.position().byte_index(),
                    "clip: expected a length",
                ));
            }
        });
    }
    input.skip_whitespace();
    input
        .expect_exhausted()
        .map_err(|e| convert_basic_error_at(input.position().byte_index(), e))?;
    match (function, edges.as_slice()) {
        ("rect", [t, r, b, l]) => Ok([*t, *r, *b, *l]),
        ("inset", [v]) => Ok([*v; 4]),
        ("inset", [v, h]) => Ok([*v, *h, *v, *h]),
        ("inset", [t, h, b]) => Ok([*t, *h, *b, *h]),
        ("inset", [t, r, b, l]) => Ok([*t, *r, *b, *l]),
        _ => Err(ParseError::new(
            input.position().byte_index(),
            "clip: wrong number of edges",
        )),
    }
}

fn parse_rgb_function<'i>(input: &mut CssParser<'i>, has_alpha: bool) -> Result<Value, ParseError> {
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

fn parse_color_byte<'i>(input: &mut CssParser<'i>) -> Result<u8, ParseError> {
    let value = parse_unsigned_number(input)?;
    Ok(value.clamp(0.0, 255.0).round() as u8)
}

fn parse_unsigned_number<'i>(input: &mut CssParser<'i>) -> Result<f32, ParseError> {
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

fn expect_token_comma<'i>(input: &mut CssParser<'i>) -> Result<(), ParseError> {
    input.skip_whitespace();
    let pos = input.position().byte_index();
    input
        .expect_comma()
        .map_err(|err| convert_basic_error_at(pos, err))
}

fn parse_url_function<'i>(input: &mut CssParser<'i>) -> Result<Value, ParseError> {
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
fn parse_var_function<'i>(input: &mut CssParser<'i>) -> Result<Value, ParseError> {
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
    let fallback = if input.try_parse(|p| p.expect_comma()).is_ok() {
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
pub(super) fn parse_length_or_number<'i>(input: &mut CssParser<'i>) -> Result<Value, ParseError> {
    input.skip_whitespace();
    let pos = input.position().byte_index();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(pos, err))?
        .clone();
    match token {
        Token::Number { value, .. } => Ok(Value::Number(value)),
        Token::Percentage { unit_value, .. } => {
            Ok(Value::Length(unit_value * 100.0, Unit::Percent))
        }
        Token::Dimension { value, unit, .. } => Ok(length_with_unit(value, unit.as_ref())),
        other => Err(token_error(input, &other, "expected a length or number")),
    }
}

pub(super) fn parse_length_token<'i>(input: &mut CssParser<'i>) -> Result<f32, ParseError> {
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
pub(super) fn peek_starts_length<'i>(input: &mut CssParser<'i>) -> bool {
    let saved = input.state();
    let result = matches!(
        input.next(),
        Ok(Token::Number { .. }) | Ok(Token::Dimension { .. }) | Ok(Token::Percentage { .. })
    );
    input.reset(&saved);
    result
}
