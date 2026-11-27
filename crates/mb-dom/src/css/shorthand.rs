// Shorthand declaration expanders. Each function takes the trailing
// value-side of a `name: ...` declaration and returns the per-property
// longhand declarations the cascade actually reads. The cascade dispatches
// on the declaration name in `parse_declaration_value`; everything that
// expands to multiple longhands lives here.

use cssparser::{Parser as CssParser, Token};

use super::error::convert_basic_error_at;
use super::parse::{length_with_unit, parse_length_or_number, parse_value, peek_starts_length};
use super::{BackgroundSize, Declaration, ParseError, SizeAxis, Unit, Value};

// -----------------------------------------------------------------------------
// border-radius
// -----------------------------------------------------------------------------

pub(super) fn parse_border_radius_shorthand<'i>(
    name: &str,
    input: &mut CssParser<'i>,
) -> Result<Vec<Declaration>, ParseError> {
    let _ = name; // shorthand always expands to fixed longhand names
    let mut lengths = Vec::new();
    loop {
        input.skip_whitespace();
        if !peek_starts_length(input) {
            break;
        }
        lengths.push(parse_length_or_number(input)?);
        if lengths.len() == 4 {
            break;
        }
    }

    if lengths.is_empty() {
        return Err(ParseError::new(
            input.position().byte_index(),
            "border-radius requires at least one length",
        ));
    }

    let (tl, tr, br, bl) = match lengths.as_slice() {
        [v] => (v.clone(), v.clone(), v.clone(), v.clone()),
        [tl_br, tr_bl] => (tl_br.clone(), tr_bl.clone(), tl_br.clone(), tr_bl.clone()),
        [tl, tr_bl, br] => (tl.clone(), tr_bl.clone(), br.clone(), tr_bl.clone()),
        [tl, tr, br, bl] => (tl.clone(), tr.clone(), br.clone(), bl.clone()),
        _ => unreachable!(),
    };

    Ok(vec![
        Declaration {
            name: "border-top-left-radius".into(),
            value: tl,
            important: false,
        },
        Declaration {
            name: "border-top-right-radius".into(),
            value: tr,
            important: false,
        },
        Declaration {
            name: "border-bottom-right-radius".into(),
            value: br,
            important: false,
        },
        Declaration {
            name: "border-bottom-left-radius".into(),
            value: bl,
            important: false,
        },
    ])
}

// -----------------------------------------------------------------------------
// padding / margin (CSS clockwise convention)
// -----------------------------------------------------------------------------

/// Expand `padding: 2px` / `margin: 8px 4px` etc. into the four per-side
/// longhands (`padding-top`, `-right`, `-bottom`, `-left`). Phase 6.K
/// catches the HN homepage's `<td style="padding: 2px">` and the orange
/// header's `<table style="padding:2px">` — both shorthand-only forms
/// the toy parser previously dropped on the floor (the value landed
/// under the literal `padding` key, which the cascade never reads).
///
/// `margin` accepts the same value grammar; `auto` is also legal there
/// per spec (used for horizontal centering) and folds through as a
/// `Keyword("auto")` so layout's existing auto-margin path picks it up.
pub(super) fn parse_box_edge_shorthand<'i>(
    name: &str,
    input: &mut CssParser<'i>,
) -> Result<Vec<Declaration>, ParseError> {
    let mut values: Vec<Value> = Vec::new();
    let allow_auto = name == "margin";
    loop {
        input.skip_whitespace();
        if input.is_exhausted() {
            break;
        }
        // `margin: auto` and `margin: 0 auto` are common; only attempt
        // the keyword path on margin so we don't accidentally accept
        // `padding: auto` (which has no spec meaning).
        if allow_auto {
            let probe = input.state();
            if let Ok(Token::Ident(ident)) = input.next()
                && ident.as_ref() == "auto"
            {
                values.push(Value::Keyword("auto".into()));
                if values.len() == 4 {
                    break;
                }
                continue;
            }
            input.reset(&probe);
        }
        if !peek_starts_length(input) {
            break;
        }
        // CSS spec: bare zero is the only unitless number that's legal
        // in a length context (`margin: 0 auto`). Promote it to a Px
        // length here so the cascade / layout always consume a Length,
        // not a Number — `lpa_or_zero` would silently treat any other
        // unitless number as zero, which would mask malformed input.
        let value = match parse_length_or_number(input)? {
            Value::Number(0.0) => Value::Length(0.0, Unit::Px),
            other => other,
        };
        values.push(value);
        if values.len() == 4 {
            break;
        }
    }

    if values.is_empty() {
        return Err(ParseError::new(
            input.position().byte_index(),
            format!("{name} requires at least one value"),
        ));
    }

    let (top, right, bottom, left) = match values.as_slice() {
        [v] => (v.clone(), v.clone(), v.clone(), v.clone()),
        [v0, v1] => (v0.clone(), v1.clone(), v0.clone(), v1.clone()),
        [v0, v1, v2] => (v0.clone(), v1.clone(), v2.clone(), v1.clone()),
        [v0, v1, v2, v3] => (v0.clone(), v1.clone(), v2.clone(), v3.clone()),
        _ => unreachable!(),
    };

    Ok(vec![
        Declaration {
            name: format!("{name}-top"),
            value: top,
            important: false,
        },
        Declaration {
            name: format!("{name}-right"),
            value: right,
            important: false,
        },
        Declaration {
            name: format!("{name}-bottom"),
            value: bottom,
            important: false,
        },
        Declaration {
            name: format!("{name}-left"),
            value: left,
            important: false,
        },
    ])
}

// -----------------------------------------------------------------------------
// flex
// -----------------------------------------------------------------------------

pub(super) fn parse_flex_shorthand<'i>(
    input: &mut CssParser<'i>,
) -> Result<Vec<Declaration>, ParseError> {
    // `none | [ <grow> <shrink>? || <basis> ]` (CSS Flexbox §7.1.1, step 1b).
    // An omitted basis is `0%` when a grow factor is given (`flex: 1` =
    // `1 1 0%`), an omitted grow/shrink is 1. A bare `0` after both factors
    // is the basis (`flex: 1 1 0`).
    let keyword = input.try_parse(|i| i.expect_ident_cloned());
    if let Ok(keyword) = &keyword {
        let lower = keyword.to_ascii_lowercase();
        let triple = match lower.as_str() {
            "none" => Some((0.0, 0.0)),
            "initial" => Some((0.0, 1.0)),
            "auto" if input.is_exhausted() => Some((1.0, 1.0)),
            _ => None,
        };
        if let Some((grow, shrink)) = triple {
            input
                .expect_exhausted()
                .map_err(|e| convert_basic_error_at(0, e))?;
            return Ok(flex_decls(grow, shrink, Value::Keyword("auto".into())));
        }
    }
    let error = |input: &CssParser<'i>, message: &str| {
        Err(ParseError::new(
            input.position().byte_index(),
            format!("flex shorthand: {message}"),
        ))
    };
    let mut grow: Option<f32> = None;
    let mut shrink: Option<f32> = None;
    let mut basis: Option<Value> = match keyword {
        Ok(keyword)
            if keyword.eq_ignore_ascii_case("auto") || keyword.eq_ignore_ascii_case("content") =>
        {
            Some(Value::Keyword(keyword.to_ascii_lowercase()))
        }
        Ok(_) => return error(input, "unknown keyword"),
        Err(_) => None,
    };
    // Whether the previous component was the grow factor (shrink must
    // follow it directly).
    let mut after_grow = false;
    loop {
        input.skip_whitespace();
        if input.is_exhausted() {
            break;
        }
        if basis.is_none()
            && let Ok(keyword) = input.try_parse(|i| i.expect_ident_cloned())
        {
            if !(keyword.eq_ignore_ascii_case("auto") || keyword.eq_ignore_ascii_case("content")) {
                return error(input, "unknown keyword");
            }
            basis = Some(Value::Keyword(keyword.to_ascii_lowercase()));
            after_grow = false;
            continue;
        }
        if !peek_starts_length(input) {
            return error(input, "unexpected token");
        }
        match parse_length_or_number(input)? {
            Value::Number(n) if grow.is_none() => {
                grow = Some(n);
                after_grow = true;
                continue;
            }
            Value::Number(n) if shrink.is_none() && after_grow => shrink = Some(n),
            Value::Number(0.0) if basis.is_none() => basis = Some(Value::Length(0.0, Unit::Px)),
            length @ Value::Length(_, _) if basis.is_none() => basis = Some(length),
            _ => return error(input, "too many values"),
        }
        after_grow = false;
    }
    if grow.is_none() && basis.is_none() {
        return error(input, "requires a factor or a basis");
    }
    let grow_value = grow.unwrap_or(1.0);
    let shrink_value = shrink.unwrap_or(1.0);
    // Negative flex factors are invalid — the whole shorthand drops.
    if grow_value < 0.0 || shrink_value < 0.0 {
        return error(input, "negative flex factors are invalid");
    }
    let basis = basis.unwrap_or(Value::Length(0.0, Unit::Percent));
    Ok(flex_decls(grow_value, shrink_value, basis))
}

fn flex_decls(grow: f32, shrink: f32, basis: Value) -> Vec<Declaration> {
    vec![
        decl("flex-grow", Value::Number(grow)),
        decl("flex-shrink", Value::Number(shrink)),
        decl("flex-basis", basis),
    ]
}

fn decl(name: &str, value: Value) -> Declaration {
    Declaration {
        name: name.to_string(),
        value,
        important: false,
    }
}

// -----------------------------------------------------------------------------
// background + background-position
// -----------------------------------------------------------------------------

/// Expand `background: <bg-image> | <bg-color> | <repeat> | <position>` into
/// the longhand declarations the cascade actually reads. The MVP handles the
/// two pieces real pages care about — the color (`background-color`) and the
/// image (`background-image`, either `url(...)` or a `linear-gradient(...)`)
/// — and discards everything else (`no-repeat`, position keywords / percents,
/// `repeat-x`, etc.). Without this expansion HN's `.votearrow { background:
/// url(grayarrow.gif) no-repeat; }` silently dropped because cssparser
/// errored on the trailing tokens after the URL, which left the painter
/// with no image to draw.
pub(super) fn parse_background_shorthand<'i>(
    input: &mut CssParser<'i>,
) -> Result<Vec<Declaration>, ParseError> {
    // `<color> || <image> || <repeat> || <position> [/ <size>]?` and the
    // box keywords (step 1b). Like Chrome, the shorthand resets what it
    // doesn't name: an omitted color is `transparent`, an omitted image
    // `none`. Position and size are emitted only when written (an omitted
    // one would reset to `0 0` / `auto` — not modelled, so earlier
    // longhands survive; a known approximation).
    let mut color: Option<Value> = None;
    let mut image: Option<Value> = None;
    let mut position: Vec<Value> = Vec::new();
    let mut size: Option<Value> = None;
    loop {
        input.skip_whitespace();
        if input.is_exhausted() {
            break;
        }
        if input.try_parse(|i| i.expect_delim('/')).is_ok() {
            if position.is_empty() || size.is_some() {
                return Err(ParseError::new(
                    input.position().byte_index(),
                    "background: `/ size` must follow a position",
                ));
            }
            size = Some(parse_background_size_layer(input)?);
            continue;
        }
        let start = input.position().byte_index();
        let value = parse_value(input)?;
        match value {
            Value::Color(_) if color.is_none() => color = Some(value),
            Value::ImageUrl(_) | Value::Gradient(_) if image.is_none() => image = Some(value),
            Value::Keyword(ref k) if k.eq_ignore_ascii_case("none") && image.is_none() => {
                image = Some(Value::Keyword("none".into()))
            }
            Value::Length(_, _) | Value::Number(_) if position.len() < 2 && size.is_none() => {
                position.push(match value {
                    Value::Number(0.0) => Value::Length(0.0, Unit::Px),
                    Value::Number(_) => {
                        return Err(ParseError::new(start, "background: unitless position"));
                    }
                    other => other,
                });
            }
            Value::Keyword(ref k)
                if is_position_keyword(k) && position.len() < 2 && size.is_none() =>
            {
                position.push(position_keyword(k));
            }
            // repeat / attachment / box keywords: accepted, not modelled.
            Value::Keyword(ref k) if is_background_keyword(k) => {}
            other => {
                return Err(ParseError::new(
                    start,
                    format!("background: unexpected {other:?}"),
                ));
            }
        }
    }
    let mut decls = vec![
        decl(
            "background-color",
            color.unwrap_or(Value::Color(super::Color {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            })),
        ),
        decl(
            "background-image",
            image.unwrap_or(Value::Keyword("none".into())),
        ),
    ];
    if !position.is_empty() {
        let x = position[0].clone();
        // One value: the other axis is `center`.
        let y = position
            .get(1)
            .cloned()
            .unwrap_or(Value::Length(50.0, Unit::Percent));
        decls.push(decl("background-position-x", x));
        decls.push(decl("background-position-y", y));
    }
    if let Some(size) = size {
        decls.push(decl("background-size", size));
    }
    Ok(decls)
}

fn is_position_keyword(keyword: &str) -> bool {
    matches!(
        keyword.to_ascii_lowercase().as_str(),
        "left" | "center" | "right" | "top" | "bottom"
    )
}

fn position_keyword(keyword: &str) -> Value {
    let percent = match keyword.to_ascii_lowercase().as_str() {
        "left" | "top" => 0.0,
        "right" | "bottom" => 100.0,
        _ => 50.0,
    };
    Value::Length(percent, Unit::Percent)
}

fn is_background_keyword(keyword: &str) -> bool {
    matches!(
        keyword.to_ascii_lowercase().as_str(),
        "repeat"
            | "no-repeat"
            | "repeat-x"
            | "repeat-y"
            | "space"
            | "round"
            | "scroll"
            | "fixed"
            | "local"
            | "border-box"
            | "padding-box"
            | "content-box"
            | "text"
    )
}

/// `background-size` (step 1b): `cover | contain | <axis> <axis>?` per
/// layer; a comma list keeps the first layer (one background image is
/// modelled) but must parse throughout.
pub(super) fn parse_background_size<'i>(input: &mut CssParser<'i>) -> Result<Value, ParseError> {
    let first = parse_background_size_layer(input)?;
    loop {
        input.skip_whitespace();
        if input.is_exhausted() {
            return Ok(first);
        }
        input
            .expect_comma()
            .map_err(|e| convert_basic_error_at(input.position().byte_index(), e))?;
        parse_background_size_layer(input)?;
    }
}

fn parse_background_size_layer<'i>(input: &mut CssParser<'i>) -> Result<Value, ParseError> {
    input.skip_whitespace();
    if let Ok(keyword) = input.try_parse(|i| -> Result<String, ()> {
        let ident = i.expect_ident_cloned().map_err(|_| ())?;
        let lower = ident.to_ascii_lowercase();
        if lower == "cover" || lower == "contain" {
            Ok(lower)
        } else {
            Err(())
        }
    }) {
        return Ok(Value::BackgroundSize(if keyword == "cover" {
            BackgroundSize::Cover
        } else {
            BackgroundSize::Contain
        }));
    }
    let width = parse_size_axis(input)?;
    input.skip_whitespace();
    let height = input.try_parse(parse_size_axis).unwrap_or(SizeAxis::Auto);
    Ok(Value::BackgroundSize(BackgroundSize::Size(width, height)))
}

fn parse_size_axis<'i>(input: &mut CssParser<'i>) -> Result<SizeAxis, ParseError> {
    input.skip_whitespace();
    let position = input.position().byte_index();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(position, err))?
        .clone();
    match token {
        Token::Ident(ident) if ident.eq_ignore_ascii_case("auto") => Ok(SizeAxis::Auto),
        Token::Number { value: 0.0, .. } => Ok(SizeAxis::Length(0.0, Unit::Px)),
        Token::Percentage { unit_value, .. } if unit_value >= 0.0 => {
            Ok(SizeAxis::Length(unit_value * 100.0, Unit::Percent))
        }
        Token::Dimension { value, unit, .. } if value >= 0.0 => {
            match length_with_unit(value, unit.as_ref()) {
                Value::Length(v, unit) => Ok(SizeAxis::Length(v, unit)),
                _ => Err(ParseError::new(position, "background-size: unknown unit")),
            }
        }
        _ => Err(ParseError::new(
            position,
            "background-size: expected a size",
        )),
    }
}

/// Expand `background-position: <x> <y>` into the per-axis longhands
/// `background-position-x` and `background-position-y`. The toy renderer
/// only acts on numeric (length / percentage) values today — the keyword
/// forms (`top`, `left`, `center`, etc.) drop through to a no-op so the
/// declaration still validates without contributing a longhand. HN's
/// vote arrow uses `background-position: 0 -10px` to slice a vertical
/// sprite strip, which the longhands let `background_image_command`
/// pick up at paint time.
pub(super) fn parse_background_position<'i>(
    input: &mut CssParser<'i>,
) -> Result<Vec<Declaration>, ParseError> {
    input.skip_whitespace();
    let x = parse_position_axis(input)?;
    input.skip_whitespace();
    // Spec: an omitted second value defaults to `center` (= 50%). We don't
    // model keywords yet, so a missing second value falls back to 0 — the
    // dominant case for sprite slicing where the strip is vertical.
    let y = if input.is_exhausted() {
        Value::Length(0.0, Unit::Px)
    } else {
        parse_position_axis(input)?
    };
    Ok(vec![
        Declaration {
            name: "background-position-x".into(),
            value: x,
            important: false,
        },
        Declaration {
            name: "background-position-y".into(),
            value: y,
            important: false,
        },
    ])
}

fn parse_position_axis<'i>(input: &mut CssParser<'i>) -> Result<Value, ParseError> {
    let probe = input.state();
    let position = input.position();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(position.byte_index(), err))?
        .clone();
    match token {
        Token::Dimension { value, unit, .. } => Ok(length_with_unit(value, unit.as_ref())),
        Token::Number { value, .. } => Ok(Value::Length(value, Unit::Px)),
        Token::Percentage { unit_value, .. } => {
            Ok(Value::Length(unit_value * 100.0, Unit::Percent))
        }
        // Keyword positions are accepted but not yet mapped to lengths;
        // the renderer falls back to (0, 0) for them.
        Token::Ident(name) => Ok(Value::Keyword(name.to_string())),
        _ => {
            input.reset(&probe);
            Err(ParseError::new(
                position.byte_index(),
                "expected length / percentage / keyword in background-position",
            ))
        }
    }
}

// -----------------------------------------------------------------------------
// border family (step 1b) — on the existing key scheme: widths under
// `border-{side}`, styles `border-{side}-style`, colours
// `border-{side}-color` (the painter prefers these over `border-color`).
// -----------------------------------------------------------------------------

const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

const BORDER_STYLES: &[&str] = &[
    "none", "hidden", "dotted", "dashed", "solid", "double", "groove", "ridge", "inset", "outset",
];

/// `border` / `border-{side}`: `<width> || <style> || <color>`, any order,
/// each at most once; an omitted part resets to its initial value
/// (`medium` = 3px, `none`, `currentcolor`).
pub(super) fn parse_border_shorthand<'i>(
    name: &str,
    input: &mut CssParser<'i>,
) -> Result<Vec<Declaration>, ParseError> {
    let mut width: Option<Value> = None;
    let mut style: Option<String> = None;
    let mut color: Option<Value> = None;
    loop {
        input.skip_whitespace();
        if input.is_exhausted() {
            break;
        }
        let start = input.position().byte_index();
        if style.is_none()
            && let Ok(s) = input.try_parse(parse_border_style)
        {
            style = Some(s);
            continue;
        }
        if width.is_none()
            && let Ok(w) = input.try_parse(parse_border_width)
        {
            width = Some(w);
            continue;
        }
        if color.is_none()
            && let Ok(c) = input.try_parse(parse_border_color)
        {
            color = Some(c);
            continue;
        }
        return Err(ParseError::new(start, format!("{name}: unexpected value")));
    }
    if width.is_none() && style.is_none() && color.is_none() {
        return Err(ParseError::new(
            input.position().byte_index(),
            format!("{name}: empty"),
        ));
    }
    let width = width.unwrap_or(Value::Length(3.0, Unit::Px));
    let style = Value::Keyword(style.unwrap_or_else(|| "none".into()));
    let color = color.unwrap_or(Value::Keyword("currentcolor".into()));
    let sides: Vec<&str> = match name.strip_prefix("border-") {
        Some(side) => vec![side],
        None => SIDES.to_vec(),
    };
    let mut decls = Vec::new();
    for side in sides {
        decls.push(decl(&format!("border-{side}"), width.clone()));
        decls.push(decl(&format!("border-{side}-style"), style.clone()));
        decls.push(decl(&format!("border-{side}-color"), color.clone()));
    }
    Ok(decls)
}

/// `border-width` / `border-style` / `border-color`: 1–4 values, top /
/// right / bottom / left as for `margin`.
pub(super) fn parse_border_sides<'i>(
    name: &str,
    input: &mut CssParser<'i>,
) -> Result<Vec<Declaration>, ParseError> {
    let mut values = Vec::new();
    loop {
        input.skip_whitespace();
        if input.is_exhausted() || values.len() == 4 {
            break;
        }
        let value = match name {
            "border-width" => parse_border_width(input)?,
            "border-style" => Value::Keyword(parse_border_style(input)?),
            _ => parse_border_color(input)?,
        };
        values.push(value);
    }
    input
        .expect_exhausted()
        .map_err(|e| convert_basic_error_at(input.position().byte_index(), e))?;
    let [top, right, bottom, left] = four_sides(values).ok_or_else(|| {
        ParseError::new(input.position().byte_index(), format!("{name}: no value"))
    })?;
    let key = |side: &str| match name {
        "border-width" => format!("border-{side}"),
        "border-style" => format!("border-{side}-style"),
        _ => format!("border-{side}-color"),
    };
    Ok(vec![
        decl(&key("top"), top),
        decl(&key("right"), right),
        decl(&key("bottom"), bottom),
        decl(&key("left"), left),
    ])
}

/// `border-{side}-width` folds onto the width key `border-{side}`.
pub(super) fn parse_border_side_width<'i>(
    side: &str,
    input: &mut CssParser<'i>,
) -> Result<Vec<Declaration>, ParseError> {
    Ok(vec![decl(
        &format!("border-{side}"),
        parse_border_width(input)?,
    )])
}

/// `inset`: 1–4 of `top/right/bottom/left` (lengths, percentages, `auto`).
pub(super) fn parse_inset_shorthand<'i>(
    input: &mut CssParser<'i>,
) -> Result<Vec<Declaration>, ParseError> {
    let mut values = Vec::new();
    loop {
        input.skip_whitespace();
        if input.is_exhausted() || values.len() == 4 {
            break;
        }
        if input.try_parse(|i| i.expect_ident_matching("auto")).is_ok() {
            values.push(Value::Keyword("auto".into()));
            continue;
        }
        values.push(match parse_length_or_number(input)? {
            Value::Number(0.0) => Value::Length(0.0, Unit::Px),
            length @ Value::Length(_, _) => length,
            _ => {
                return Err(ParseError::new(
                    input.position().byte_index(),
                    "inset: expected a length",
                ));
            }
        });
    }
    input
        .expect_exhausted()
        .map_err(|e| convert_basic_error_at(input.position().byte_index(), e))?;
    let [top, right, bottom, left] = four_sides(values)
        .ok_or_else(|| ParseError::new(input.position().byte_index(), "inset: no value"))?;
    Ok(vec![
        decl("top", top),
        decl("right", right),
        decl("bottom", bottom),
        decl("left", left),
    ])
}

fn four_sides(values: Vec<Value>) -> Option<[Value; 4]> {
    Some(match values.as_slice() {
        [v] => [v.clone(), v.clone(), v.clone(), v.clone()],
        [v0, v1] => [v0.clone(), v1.clone(), v0.clone(), v1.clone()],
        [v0, v1, v2] => [v0.clone(), v1.clone(), v2.clone(), v1.clone()],
        [v0, v1, v2, v3] => [v0.clone(), v1.clone(), v2.clone(), v3.clone()],
        _ => return None,
    })
}

fn parse_border_style<'i>(input: &mut CssParser<'i>) -> Result<String, ParseError> {
    let position = input.position().byte_index();
    let ident = input
        .expect_ident_cloned()
        .map_err(|e| convert_basic_error_at(position, e))?;
    let lower = ident.to_ascii_lowercase();
    if BORDER_STYLES.contains(&lower.as_str()) {
        Ok(lower)
    } else {
        Err(ParseError::new(position, "border style: unknown keyword"))
    }
}

/// A border width: a non-negative length, a bare `0`, or `thin` /
/// `medium` / `thick` (1 / 3 / 5px, as Chrome).
fn parse_border_width<'i>(input: &mut CssParser<'i>) -> Result<Value, ParseError> {
    input.skip_whitespace();
    let position = input.position().byte_index();
    let token = input
        .next()
        .map_err(|err| convert_basic_error_at(position, err))?
        .clone();
    let px = |v: f32| Ok(Value::Length(v, Unit::Px));
    match token {
        Token::Ident(ident) => match ident.to_ascii_lowercase().as_str() {
            "thin" => px(1.0),
            "medium" => px(3.0),
            "thick" => px(5.0),
            _ => Err(ParseError::new(position, "border width: unknown keyword")),
        },
        Token::Number { value: 0.0, .. } => px(0.0),
        Token::Dimension { value, unit, .. } if value >= 0.0 => {
            match length_with_unit(value, unit.as_ref()) {
                length @ Value::Length(_, _) => Ok(length),
                _ => Err(ParseError::new(position, "border width: unknown unit")),
            }
        }
        _ => Err(ParseError::new(position, "border width: expected a length")),
    }
}

fn parse_border_color<'i>(input: &mut CssParser<'i>) -> Result<Value, ParseError> {
    input.skip_whitespace();
    let position = input.position().byte_index();
    match parse_value(input)? {
        color @ Value::Color(_) => Ok(color),
        Value::Keyword(k) if k.eq_ignore_ascii_case("currentcolor") => {
            Ok(Value::Keyword("currentcolor".into()))
        }
        other => Err(ParseError::new(
            position,
            format!("border color: got {other:?}"),
        )),
    }
}
