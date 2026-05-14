// Shorthand declaration expanders. Each function takes the trailing
// value-side of a `name: ...` declaration and returns the per-property
// longhand declarations the cascade actually reads. The cascade dispatches
// on the declaration name in `parse_declaration_value`; everything that
// expands to multiple longhands lives here.

use cssparser::{Parser as CssParser, Token};

use super::error::convert_basic_error_at;
use super::parse::{length_with_unit, parse_length_or_number, parse_value, peek_starts_length};
use super::{Declaration, ParseError, Unit, Value};

// -----------------------------------------------------------------------------
// border-radius
// -----------------------------------------------------------------------------

pub(super) fn parse_border_radius_shorthand<'i, 't>(
    name: &str,
    input: &mut CssParser<'i, 't>,
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
        },
        Declaration {
            name: "border-top-right-radius".into(),
            value: tr,
        },
        Declaration {
            name: "border-bottom-right-radius".into(),
            value: br,
        },
        Declaration {
            name: "border-bottom-left-radius".into(),
            value: bl,
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
pub(super) fn parse_box_edge_shorthand<'i, 't>(
    name: &str,
    input: &mut CssParser<'i, 't>,
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
            Value::Number(n) if n == 0.0 => Value::Length(0.0, Unit::Px),
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
        },
        Declaration {
            name: format!("{name}-right"),
            value: right,
        },
        Declaration {
            name: format!("{name}-bottom"),
            value: bottom,
        },
        Declaration {
            name: format!("{name}-left"),
            value: left,
        },
    ])
}

// -----------------------------------------------------------------------------
// flex
// -----------------------------------------------------------------------------

pub(super) fn parse_flex_shorthand<'i, 't>(
    input: &mut CssParser<'i, 't>,
) -> Result<Vec<Declaration>, ParseError> {
    // Same grammar as the original parser — see the long comment in the prior
    // implementation. We greedily consume up to three numeric tokens and stop
    // when we see a length (which becomes the basis) or run out.
    let mut grow: Option<f32> = None;
    let mut shrink: Option<f32> = None;
    let mut basis: Option<Value> = None;

    for _slot in 0..3 {
        input.skip_whitespace();
        if !peek_starts_length(input) {
            break;
        }
        match parse_length_or_number(input)? {
            Value::Number(n) => {
                if grow.is_none() {
                    grow = Some(n);
                } else if shrink.is_none() {
                    shrink = Some(n);
                } else {
                    return Err(ParseError::new(
                        input.position().byte_index(),
                        "flex shorthand: third value must be a length",
                    ));
                }
            }
            length @ Value::Length(_, _) => {
                basis = Some(length);
                break;
            }
            _ => break,
        }
    }

    if grow.is_none() && basis.is_none() {
        return Err(ParseError::new(
            input.position().byte_index(),
            "flex shorthand requires at least one number or length",
        ));
    }

    let grow_value = grow.unwrap_or(1.0);
    let shrink_value = shrink.unwrap_or(1.0);
    let mut decls = vec![
        Declaration {
            name: "flex-grow".into(),
            value: Value::Number(grow_value),
        },
        Declaration {
            name: "flex-shrink".into(),
            value: Value::Number(shrink_value),
        },
    ];
    if let Some(basis_value) = basis {
        decls.push(Declaration {
            name: "flex-basis".into(),
            value: basis_value,
        });
    }
    Ok(decls)
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
pub(super) fn parse_background_shorthand<'i, 't>(
    input: &mut CssParser<'i, 't>,
) -> Result<Vec<Declaration>, ParseError> {
    let mut color: Option<Value> = None;
    let mut image: Option<Value> = None;

    loop {
        input.skip_whitespace();
        let probe = input.state();
        let value = match parse_value(input) {
            Ok(v) => v,
            Err(_) => {
                // EOF or token shape we don't understand — restore the
                // input cursor so the surrounding declaration block parser
                // can continue past this declaration without choking.
                input.reset(&probe);
                break;
            }
        };
        match value {
            Value::Color(_) if color.is_none() => color = Some(value),
            Value::ImageUrl(_) | Value::Gradient(_) if image.is_none() => image = Some(value),
            // Position keywords (`top`, `left`, `center`, …), repeat
            // keywords (`no-repeat`, `repeat-x`, …), positions written
            // as lengths/percentages, and the `none` / `transparent`
            // sentinels all land here. Real CSS would route them into
            // the per-axis longhands; keeping them as a no-op is the
            // pragmatic shape until a page actually needs them.
            _ => {}
        }
    }

    let mut decls = Vec::new();
    if let Some(c) = color {
        decls.push(Declaration {
            name: "background-color".into(),
            value: c,
        });
    }
    if let Some(i) = image {
        decls.push(Declaration {
            name: "background-image".into(),
            value: i,
        });
    }
    if decls.is_empty() {
        return Err(ParseError::new(
            input.position().byte_index(),
            "background shorthand requires at least a color or image",
        ));
    }
    Ok(decls)
}

/// Expand `background-position: <x> <y>` into the per-axis longhands
/// `background-position-x` and `background-position-y`. The toy renderer
/// only acts on numeric (length / percentage) values today — the keyword
/// forms (`top`, `left`, `center`, etc.) drop through to a no-op so the
/// declaration still validates without contributing a longhand. HN's
/// vote arrow uses `background-position: 0 -10px` to slice a vertical
/// sprite strip, which the longhands let `background_image_command`
/// pick up at paint time.
pub(super) fn parse_background_position<'i, 't>(
    input: &mut CssParser<'i, 't>,
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
        },
        Declaration {
            name: "background-position-y".into(),
            value: y,
        },
    ])
}

fn parse_position_axis<'i, 't>(input: &mut CssParser<'i, 't>) -> Result<Value, ParseError> {
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
