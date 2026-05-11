// Presentational HTML attribute translation. Maps the legacy
// `bgcolor`/`width`/`align`/`valign`/`border`/`cellspacing` attributes
// (and their `<font color>` cousins) into the equivalent CSS
// declarations, so the cascade can apply them just like any other
// stylesheet rule. The mapping runs at style time — once they land in
// the cascade as Specified Values, the rest of the engine (layout /
// render / queries) stays attribute-blind.
//
// Author CSS still wins because presentational hints are applied
// before matched declarations.

use crate::{css::Value, dom::ElementData};

use super::PropertyMap;

pub(super) fn presentational_hints(element: &ElementData) -> PropertyMap {
    let mut hints = PropertyMap::new();
    let tag = element.tag_name.as_str();

    // bgcolor → background-color, on every tag the historical HTML 4 spec
    // accepted (body/table/tr/td/th most often, but real pages put it on
    // <div> too). Keeping it tag-agnostic avoids whitelist drift.
    if let Some(color_str) = element.attributes.get("bgcolor")
        && let Some(color) = parse_html_color(color_str)
    {
        hints.insert("background-color".into(), Value::Color(color));
    }

    // <font color="..."> / <basefont color="..."> map to CSS color. Other tags
    // never used a `color` attribute, so the whitelist keeps a regular
    // `color="..."` on, say, an icon button from leaking into text color.
    if matches!(tag, "font" | "basefont")
        && let Some(color_str) = element.attributes.get("color")
        && let Some(color) = parse_html_color(color_str)
    {
        hints.insert("color".into(), Value::Color(color));
    }

    // width / height attribute → CSS width / height. Whitelisted to the tags
    // that historically accepted them as presentational hints; a stray
    // `<input width="...">` shouldn't suddenly resize the input via this
    // path (UA defaults already give inputs a fixed width).
    if matches!(
        tag,
        "img"
            | "table"
            | "td"
            | "th"
            | "col"
            | "colgroup"
            | "hr"
            | "iframe"
            | "video"
            | "canvas"
            | "embed"
            | "object"
    ) {
        if let Some(value) = element
            .attributes
            .get("width")
            .and_then(|s| parse_html_length(s))
        {
            hints.insert("width".into(), value);
        }
        if let Some(value) = element
            .attributes
            .get("height")
            .and_then(|s| parse_html_length(s))
        {
            hints.insert("height".into(), value);
        }
    }

    // align — meaning depends on the element. On floatable embeds (img,
    // table) "left"/"right" map to CSS float; on block / table-section
    // elements every keyword maps to text-align. We don't model "center"
    // for floatable embeds (modern CSS is `margin: auto`, but our layout
    // doesn't honor that automatically for floats).
    if let Some(raw_align) = element.attributes.get("align") {
        let align = raw_align.trim().to_ascii_lowercase();
        if matches!(tag, "img" | "table" | "figure")
            && matches!(align.as_str(), "left" | "right")
        {
            hints.insert("float".into(), Value::Keyword(align));
        } else if matches!(
            tag,
            "p" | "div"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "td"
                | "th"
                | "tr"
                | "tbody"
                | "thead"
                | "tfoot"
                | "caption"
        ) && matches!(align.as_str(), "left" | "right" | "center" | "justify")
        {
            hints.insert("text-align".into(), Value::Keyword(align));
        }
    }

    // valign on table cells → vertical-align. Only the four spec keywords
    // (top/middle/bottom/baseline) are accepted.
    if matches!(tag, "td" | "th" | "tr" | "tbody" | "thead" | "tfoot")
        && let Some(raw_valign) = element.attributes.get("valign")
    {
        let valign = raw_valign.trim().to_ascii_lowercase();
        if matches!(valign.as_str(), "top" | "middle" | "bottom" | "baseline") {
            hints.insert("vertical-align".into(), Value::Keyword(valign));
        }
    }

    // border on <img>/<table> → uniform border on all four sides plus a
    // solid style. `border="0"` is the most common case (image links that
    // explicitly drop the default link border) and our edge default of 0
    // already matches that, but emitting the explicit zero-length keeps
    // round-trip queries honest.
    if matches!(tag, "img" | "table")
        && let Some(width) = element
            .attributes
            .get("border")
            .and_then(|v| v.trim().parse::<f32>().ok())
    {
        for side in ["top", "right", "bottom", "left"] {
            hints.insert(
                format!("border-{side}"),
                Value::Length(width, crate::css::Unit::Px),
            );
        }
        if width > 0.0 {
            hints.insert("border-style".into(), Value::Keyword("solid".into()));
        }
    }

    // cellspacing on <table> → border-spacing. Only honored once table
    // layout lands; emitting the value now means the styled tree already
    // carries the correct number when we get there.
    if tag == "table"
        && let Some(spacing) = element
            .attributes
            .get("cellspacing")
            .and_then(|v| v.trim().parse::<f32>().ok())
    {
        hints.insert(
            "border-spacing".into(),
            Value::Length(spacing, crate::css::Unit::Px),
        );
    }

    hints
}

fn parse_html_length(raw: &str) -> Option<Value> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_suffix('%') {
        return rest.trim().parse::<f32>().ok().map(|n| Value::Length(n, crate::css::Unit::Percent));
    }
    // HTML legacy length: optional trailing "px" but commonly bare digits
    // like `width="200"`. We only accept finite, non-negative numbers; a
    // negative width isn't meaningful and a NaN would corrupt layout math.
    let stripped = trimmed.strip_suffix("px").unwrap_or(trimmed);
    stripped
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|n| n.is_finite() && *n >= 0.0)
        .map(|n| Value::Length(n, crate::css::Unit::Px))
}

fn parse_html_color(raw: &str) -> Option<crate::css::Color> {
    let trimmed = raw.trim();
    if let Some(hex) = trimmed.strip_prefix('#') {
        return parse_hex_color_body(hex);
    }
    // Legacy HTML attributes also accept bare 6-digit hex without '#'
    // (`bgcolor="ffffff"`). Try that before falling back to named colors so
    // a value like "fff" doesn't get misrouted through the named lookup.
    if let Some(color) = parse_hex_color_body(trimmed) {
        return Some(color);
    }
    named_html_color(trimmed)
}

fn parse_hex_color_body(body: &str) -> Option<crate::css::Color> {
    let bytes = body.as_bytes();
    let (r, g, b) = match bytes.len() {
        3 => (
            u8::from_str_radix(&body[0..1].repeat(2), 16).ok()?,
            u8::from_str_radix(&body[1..2].repeat(2), 16).ok()?,
            u8::from_str_radix(&body[2..3].repeat(2), 16).ok()?,
        ),
        6 => (
            u8::from_str_radix(&body[0..2], 16).ok()?,
            u8::from_str_radix(&body[2..4], 16).ok()?,
            u8::from_str_radix(&body[4..6], 16).ok()?,
        ),
        _ => return None,
    };
    Some(crate::css::Color { r, g, b, a: 255 })
}

fn named_html_color(name: &str) -> Option<crate::css::Color> {
    let lower = name.to_ascii_lowercase();
    let (r, g, b) = match lower.as_str() {
        // The HTML 4 named-color set, extended with the most common CSS
        // names that show up in legacy attributes. CSS3 has 140; this is
        // a survival subset, not a complete table.
        "black" => (0, 0, 0),
        "white" => (255, 255, 255),
        "red" => (255, 0, 0),
        "green" => (0, 128, 0),
        "lime" => (0, 255, 0),
        "blue" => (0, 0, 255),
        "yellow" => (255, 255, 0),
        "cyan" | "aqua" => (0, 255, 255),
        "magenta" | "fuchsia" => (255, 0, 255),
        "silver" => (192, 192, 192),
        "gray" | "grey" => (128, 128, 128),
        "maroon" => (128, 0, 0),
        "olive" => (128, 128, 0),
        "purple" => (128, 0, 128),
        "teal" => (0, 128, 128),
        "navy" => (0, 0, 128),
        "orange" => (255, 165, 0),
        "pink" => (255, 192, 203),
        "brown" => (165, 42, 42),
        _ => return None,
    };
    Some(crate::css::Color { r, g, b, a: 255 })
}
