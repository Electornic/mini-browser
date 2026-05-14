use super::Color;

/// Returns the rgba color associated with a CSS named color keyword, if any.
/// Currently covers the HTML4 basic palette plus a handful of common extras and
/// `transparent`. Anything outside this set falls through to the generic keyword path.
pub fn named_color(name: &str) -> Option<Color> {
    let rgba = |r: u8, g: u8, b: u8, a: u8| Color { r, g, b, a };
    match name.to_ascii_lowercase().as_str() {
        // HTML4 basic 16.
        "black" => Some(rgba(0, 0, 0, 255)),
        "silver" => Some(rgba(192, 192, 192, 255)),
        "gray" | "grey" => Some(rgba(128, 128, 128, 255)),
        "white" => Some(rgba(255, 255, 255, 255)),
        "maroon" => Some(rgba(128, 0, 0, 255)),
        "red" => Some(rgba(255, 0, 0, 255)),
        "purple" => Some(rgba(128, 0, 128, 255)),
        "fuchsia" | "magenta" => Some(rgba(255, 0, 255, 255)),
        "green" => Some(rgba(0, 128, 0, 255)),
        "lime" => Some(rgba(0, 255, 0, 255)),
        "olive" => Some(rgba(128, 128, 0, 255)),
        "yellow" => Some(rgba(255, 255, 0, 255)),
        "navy" => Some(rgba(0, 0, 128, 255)),
        "blue" => Some(rgba(0, 0, 255, 255)),
        "teal" => Some(rgba(0, 128, 128, 255)),
        "aqua" | "cyan" => Some(rgba(0, 255, 255, 255)),
        // Common extras worth shipping early since toy pages reach for them.
        "orange" => Some(rgba(255, 165, 0, 255)),
        "pink" => Some(rgba(255, 192, 203, 255)),
        "brown" => Some(rgba(165, 42, 42, 255)),
        "transparent" => Some(rgba(0, 0, 0, 0)),
        _ => None,
    }
}
