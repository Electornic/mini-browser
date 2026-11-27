//! Logical properties → physical ones (compat round 3 follow-up F1).
//!
//! The engine lays out horizontal-tb, left-to-right text only, so every
//! flow-relative property has one physical meaning: inline = left/right
//! (start = left), block = top/bottom (start = top). The parser rewrites
//! them at parse time; the cascade, layout and paint only ever see the
//! physical names. A logical and a physical declaration of the same side
//! then cascade as one property (source order / specificity decide), as
//! they do in a browser for this writing mode.

/// How a logical property maps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Logical {
    /// One physical property under another name (`inline-size` → `width`).
    Rename(&'static str),
    /// One or two values: start, end (end = start when only one is given)
    /// (`margin-inline: 1px 2px` → `margin-left: 1px; margin-right: 2px`).
    Pair(&'static str, &'static str),
    /// The whole value to both sides (`border-inline: 1px solid` →
    /// `border-left` and `border-right`).
    Both(&'static str, &'static str),
}

/// The mapping for `name` (lower-cased), or `None` for a physical name.
pub(super) fn logical(name: &str) -> Option<Logical> {
    use Logical::{Both, Pair, Rename};
    Some(match name {
        "inline-size" => Rename("width"),
        "block-size" => Rename("height"),
        "min-inline-size" => Rename("min-width"),
        "min-block-size" => Rename("min-height"),
        "max-inline-size" => Rename("max-width"),
        "max-block-size" => Rename("max-height"),
        "margin-inline-start" => Rename("margin-left"),
        "margin-inline-end" => Rename("margin-right"),
        "margin-block-start" => Rename("margin-top"),
        "margin-block-end" => Rename("margin-bottom"),
        "padding-inline-start" => Rename("padding-left"),
        "padding-inline-end" => Rename("padding-right"),
        "padding-block-start" => Rename("padding-top"),
        "padding-block-end" => Rename("padding-bottom"),
        "inset-inline-start" => Rename("left"),
        "inset-inline-end" => Rename("right"),
        "inset-block-start" => Rename("top"),
        "inset-block-end" => Rename("bottom"),
        "border-inline-start" => Rename("border-left"),
        "border-inline-end" => Rename("border-right"),
        "border-block-start" => Rename("border-top"),
        "border-block-end" => Rename("border-bottom"),
        "border-inline-start-width" => Rename("border-left-width"),
        "border-inline-end-width" => Rename("border-right-width"),
        "border-block-start-width" => Rename("border-top-width"),
        "border-block-end-width" => Rename("border-bottom-width"),
        "border-inline-start-style" => Rename("border-left-style"),
        "border-inline-end-style" => Rename("border-right-style"),
        "border-block-start-style" => Rename("border-top-style"),
        "border-block-end-style" => Rename("border-bottom-style"),
        "border-inline-start-color" => Rename("border-left-color"),
        "border-inline-end-color" => Rename("border-right-color"),
        "border-block-start-color" => Rename("border-top-color"),
        "border-block-end-color" => Rename("border-bottom-color"),
        "border-start-start-radius" => Rename("border-top-left-radius"),
        "border-start-end-radius" => Rename("border-top-right-radius"),
        "border-end-start-radius" => Rename("border-bottom-left-radius"),
        "border-end-end-radius" => Rename("border-bottom-right-radius"),
        "margin-inline" => Pair("margin-left", "margin-right"),
        "margin-block" => Pair("margin-top", "margin-bottom"),
        "padding-inline" => Pair("padding-left", "padding-right"),
        "padding-block" => Pair("padding-top", "padding-bottom"),
        "inset-inline" => Pair("left", "right"),
        "inset-block" => Pair("top", "bottom"),
        "border-inline-width" => Pair("border-left-width", "border-right-width"),
        "border-block-width" => Pair("border-top-width", "border-bottom-width"),
        "border-inline-style" => Pair("border-left-style", "border-right-style"),
        "border-block-style" => Pair("border-top-style", "border-bottom-style"),
        "border-inline-color" => Pair("border-left-color", "border-right-color"),
        "border-block-color" => Pair("border-top-color", "border-bottom-color"),
        "border-inline" => Both("border-left", "border-right"),
        "border-block" => Both("border-top", "border-bottom"),
        _ => return None,
    })
}

/// The physical properties a logical one sets, before their own longhand
/// expansion (for `longhands_of`).
pub(super) fn targets(mapping: Logical) -> Vec<&'static str> {
    match mapping {
        Logical::Rename(name) => vec![name],
        Logical::Pair(start, end) | Logical::Both(start, end) => vec![start, end],
    }
}
