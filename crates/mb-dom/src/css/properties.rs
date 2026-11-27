//! Which CSS properties exist, and what kind of value each one takes.
//!
//! The parser itself dispatches by name in `parse_declaration_value` and
//! sends everything else to the generic single-token `parse_value`, so it
//! never needed a registry. Three things do (docs/compat-round3.md):
//!
//! - the compat probe's **ours / browser-too** split of dropped declarations:
//!   a property no browser knows (a typo, `-moz-*`, an IE hack) is dropped
//!   by Chrome too, so it isn't our bug;
//! - the **`Keyword` fallback** count: a property that wants a length, a
//!   colour or one of a few keywords but kept a `Keyword` (`100vw`,
//!   `calc(…)` text, a quoted string) parsed "successfully" into something
//!   nothing downstream understands;
//! - later, `@supports (prop: value)` (step 2.3) — "known property with a
//!   meaningful value", from the same table.
//!
//! "Known" means a property Chrome accepts, not one this engine renders —
//! the table lists every standard property (most as [`ValueKind::Other`]).
//! Missing a real property here would wrongly file its drops as
//! browser-too, so when in doubt, add it. The classification built on this
//! table is frozen for compat round 3: change docs/compat-round3.md first.

/// The value shape a property takes, as far as the classification needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    /// A length or percentage, or one of the listed keywords.
    Length(&'static [&'static str]),
    /// A bare number, or one of the listed keywords.
    Number(&'static [&'static str]),
    /// A colour (`currentcolor` and friends are listed keywords).
    Color(&'static [&'static str]),
    /// Exactly one of the listed keywords.
    Keywords(&'static [&'static str]),
    /// `url(…)` / a gradient, or one of the listed keywords.
    Image(&'static [&'static str]),
    /// Parsed by its own function in `css/mod.rs` (shorthands, shadows,
    /// transforms, grid) — never stored as a fallback `Keyword`.
    Dedicated,
    /// A real property whose value shape the engine doesn't model; the
    /// generic parser keeps one token of anything.
    Other,
}

impl ValueKind {
    /// Whether `keyword` is a meaningful single-keyword value here. False
    /// for [`ValueKind::Other`]: there is nothing to judge against.
    pub fn accepts_keyword(self, keyword: &str) -> bool {
        let keyword = keyword.to_ascii_lowercase();
        if CSS_WIDE_KEYWORDS.contains(&keyword.as_str()) {
            return true;
        }
        match self {
            ValueKind::Length(list)
            | ValueKind::Number(list)
            | ValueKind::Color(list)
            | ValueKind::Keywords(list)
            | ValueKind::Image(list) => list.contains(&keyword.as_str()),
            ValueKind::Dedicated | ValueKind::Other => false,
        }
    }

    /// Whether the value shape is modelled (anything but `Other`), i.e. a
    /// stored `Keyword` outside the accepted list is a fallback.
    pub fn is_typed(self) -> bool {
        !matches!(self, ValueKind::Other)
    }
}

/// Valid for every property.
pub const CSS_WIDE_KEYWORDS: &[&str] = &["inherit", "initial", "unset", "revert", "revert-layer"];

const AUTO: &[&str] = &["auto"];
const NONE: &[&str] = &["none"];
const NORMAL: &[&str] = &["normal"];
const NO_KEYWORDS: &[&str] = &[];
const SIZE: &[&str] = &[
    "auto",
    "min-content",
    "max-content",
    "fit-content",
    "stretch",
    "-webkit-fill-available",
];
const MAX_SIZE: &[&str] = &[
    "none",
    "min-content",
    "max-content",
    "fit-content",
    "stretch",
    "-webkit-fill-available",
];
const COLOR: &[&str] = &["currentcolor", "transparent"];
const BORDER_WIDTH: &[&str] = &["thin", "medium", "thick"];
const OVERFLOW: &[&str] = &["visible", "hidden", "clip", "scroll", "auto", "overlay"];
const CONTENT_ALIGN: &[&str] = &[
    "normal",
    "start",
    "end",
    "center",
    "flex-start",
    "flex-end",
    "left",
    "right",
    "space-between",
    "space-around",
    "space-evenly",
    "stretch",
    "baseline",
];
const ITEMS_ALIGN: &[&str] = &[
    "normal",
    "stretch",
    "baseline",
    "start",
    "end",
    "center",
    "flex-start",
    "flex-end",
    "self-start",
    "self-end",
    "left",
    "right",
    "anchor-center",
];
const SELF_ALIGN: &[&str] = &[
    "auto",
    "normal",
    "stretch",
    "baseline",
    "start",
    "end",
    "center",
    "flex-start",
    "flex-end",
    "self-start",
    "self-end",
    "left",
    "right",
    "anchor-center",
];
const DISPLAY: &[&str] = &[
    "none",
    "block",
    "inline",
    "inline-block",
    "flex",
    "inline-flex",
    "grid",
    "inline-grid",
    "flow-root",
    "contents",
    "list-item",
    "table",
    "inline-table",
    "table-row",
    "table-cell",
    "table-row-group",
    "table-header-group",
    "table-footer-group",
    "table-column",
    "table-column-group",
    "table-caption",
    "ruby",
    "ruby-text",
    "math",
    "-webkit-box",
    "-webkit-inline-box",
];
const FONT_SIZE: &[&str] = &[
    "xx-small",
    "x-small",
    "small",
    "medium",
    "large",
    "x-large",
    "xx-large",
    "xxx-large",
    "smaller",
    "larger",
    "math",
];
const FONT_WEIGHT: &[&str] = &["normal", "bold", "bolder", "lighter"];
const TEXT_ALIGN: &[&str] = &[
    "start",
    "end",
    "left",
    "right",
    "center",
    "justify",
    "match-parent",
    "-webkit-center",
    "-webkit-left",
    "-webkit-right",
];
const VERTICAL_ALIGN: &[&str] = &[
    "baseline",
    "sub",
    "super",
    "text-top",
    "text-bottom",
    "middle",
    "top",
    "bottom",
];
const WHITE_SPACE: &[&str] = &[
    "normal",
    "pre",
    "nowrap",
    "pre-wrap",
    "pre-line",
    "break-spaces",
];

/// Every property Chrome knows, sorted by name (a test keeps it sorted and
/// unique, so lookup is a binary search). `-webkit-` aliases of a listed
/// property are resolved by [`lookup`], not listed.
const PROPERTIES: &[(&str, ValueKind)] = &[
    ("accent-color", ValueKind::Color(&["auto", "currentcolor"])),
    ("align-content", ValueKind::Keywords(CONTENT_ALIGN)),
    ("align-items", ValueKind::Keywords(ITEMS_ALIGN)),
    ("align-self", ValueKind::Keywords(SELF_ALIGN)),
    ("alignment-baseline", ValueKind::Other),
    ("all", ValueKind::Keywords(NO_KEYWORDS)),
    ("anchor-name", ValueKind::Other),
    ("anchor-scope", ValueKind::Other),
    ("animation", ValueKind::Other),
    ("animation-composition", ValueKind::Other),
    ("animation-delay", ValueKind::Other),
    ("animation-direction", ValueKind::Other),
    ("animation-duration", ValueKind::Other),
    ("animation-fill-mode", ValueKind::Other),
    ("animation-iteration-count", ValueKind::Other),
    ("animation-name", ValueKind::Other),
    ("animation-play-state", ValueKind::Other),
    ("animation-range", ValueKind::Other),
    ("animation-range-end", ValueKind::Other),
    ("animation-range-start", ValueKind::Other),
    ("animation-timeline", ValueKind::Other),
    ("animation-timing-function", ValueKind::Other),
    ("app-region", ValueKind::Other),
    ("appearance", ValueKind::Other),
    ("aspect-ratio", ValueKind::Other),
    ("backdrop-filter", ValueKind::Other),
    (
        "backface-visibility",
        ValueKind::Keywords(&["visible", "hidden"]),
    ),
    ("background", ValueKind::Dedicated),
    ("background-attachment", ValueKind::Other),
    ("background-blend-mode", ValueKind::Other),
    ("background-clip", ValueKind::Other),
    ("background-color", ValueKind::Color(COLOR)),
    ("background-image", ValueKind::Image(NONE)),
    ("background-origin", ValueKind::Other),
    ("background-position", ValueKind::Dedicated),
    (
        "background-position-x",
        ValueKind::Length(&["left", "center", "right"]),
    ),
    (
        "background-position-y",
        ValueKind::Length(&["top", "center", "bottom"]),
    ),
    ("background-repeat", ValueKind::Other),
    ("background-size", ValueKind::Dedicated),
    ("baseline-shift", ValueKind::Other),
    ("baseline-source", ValueKind::Other),
    ("block-size", ValueKind::Length(SIZE)),
    ("border", ValueKind::Dedicated),
    ("border-block", ValueKind::Other),
    ("border-block-color", ValueKind::Other),
    ("border-block-end", ValueKind::Other),
    ("border-block-end-color", ValueKind::Color(COLOR)),
    ("border-block-end-style", ValueKind::Other),
    ("border-block-end-width", ValueKind::Length(BORDER_WIDTH)),
    ("border-block-start", ValueKind::Other),
    ("border-block-start-color", ValueKind::Color(COLOR)),
    ("border-block-start-style", ValueKind::Other),
    ("border-block-start-width", ValueKind::Length(BORDER_WIDTH)),
    ("border-block-style", ValueKind::Other),
    ("border-block-width", ValueKind::Other),
    // The engine stores the width of `border-<side>` under the shorthand
    // name (docs/compat-round3.md step 1), so a single length is typed.
    ("border-bottom", ValueKind::Dedicated),
    ("border-bottom-color", ValueKind::Color(COLOR)),
    ("border-bottom-left-radius", ValueKind::Length(NO_KEYWORDS)),
    ("border-bottom-right-radius", ValueKind::Length(NO_KEYWORDS)),
    ("border-bottom-style", ValueKind::Other),
    ("border-bottom-width", ValueKind::Dedicated),
    (
        "border-collapse",
        ValueKind::Keywords(&["separate", "collapse"]),
    ),
    ("border-color", ValueKind::Dedicated),
    ("border-end-end-radius", ValueKind::Length(NO_KEYWORDS)),
    ("border-end-start-radius", ValueKind::Length(NO_KEYWORDS)),
    ("border-image", ValueKind::Other),
    ("border-image-outset", ValueKind::Other),
    ("border-image-repeat", ValueKind::Other),
    ("border-image-slice", ValueKind::Other),
    ("border-image-source", ValueKind::Other),
    ("border-image-width", ValueKind::Other),
    ("border-inline", ValueKind::Other),
    ("border-inline-color", ValueKind::Other),
    ("border-inline-end", ValueKind::Other),
    ("border-inline-end-color", ValueKind::Color(COLOR)),
    ("border-inline-end-style", ValueKind::Other),
    ("border-inline-end-width", ValueKind::Length(BORDER_WIDTH)),
    ("border-inline-start", ValueKind::Other),
    ("border-inline-start-color", ValueKind::Color(COLOR)),
    ("border-inline-start-style", ValueKind::Other),
    ("border-inline-start-width", ValueKind::Length(BORDER_WIDTH)),
    ("border-inline-style", ValueKind::Other),
    ("border-inline-width", ValueKind::Other),
    ("border-left", ValueKind::Dedicated),
    ("border-left-color", ValueKind::Color(COLOR)),
    ("border-left-style", ValueKind::Other),
    ("border-left-width", ValueKind::Dedicated),
    ("border-radius", ValueKind::Dedicated),
    ("border-right", ValueKind::Dedicated),
    ("border-right-color", ValueKind::Color(COLOR)),
    ("border-right-style", ValueKind::Other),
    ("border-right-width", ValueKind::Dedicated),
    ("border-spacing", ValueKind::Length(NO_KEYWORDS)),
    ("border-start-end-radius", ValueKind::Length(NO_KEYWORDS)),
    ("border-start-start-radius", ValueKind::Length(NO_KEYWORDS)),
    ("border-style", ValueKind::Dedicated),
    ("border-top", ValueKind::Dedicated),
    ("border-top-color", ValueKind::Color(COLOR)),
    ("border-top-left-radius", ValueKind::Length(NO_KEYWORDS)),
    ("border-top-right-radius", ValueKind::Length(NO_KEYWORDS)),
    ("border-top-style", ValueKind::Other),
    ("border-top-width", ValueKind::Dedicated),
    ("border-width", ValueKind::Dedicated),
    ("bottom", ValueKind::Length(AUTO)),
    ("box-decoration-break", ValueKind::Other),
    ("box-shadow", ValueKind::Dedicated),
    (
        "box-sizing",
        ValueKind::Keywords(&["content-box", "border-box"]),
    ),
    ("break-after", ValueKind::Other),
    ("break-before", ValueKind::Other),
    ("break-inside", ValueKind::Other),
    ("buffered-rendering", ValueKind::Other),
    ("caption-side", ValueKind::Other),
    (
        "caret-color",
        ValueKind::Color(&["auto", "currentcolor", "transparent"]),
    ),
    (
        "clear",
        ValueKind::Keywords(&[
            "none",
            "left",
            "right",
            "both",
            "inline-start",
            "inline-end",
        ]),
    ),
    ("clip", ValueKind::Dedicated),
    ("clip-path", ValueKind::Dedicated),
    ("clip-rule", ValueKind::Other),
    ("color", ValueKind::Color(COLOR)),
    ("color-interpolation", ValueKind::Other),
    ("color-interpolation-filters", ValueKind::Other),
    ("color-rendering", ValueKind::Other),
    ("color-scheme", ValueKind::Other),
    ("column-count", ValueKind::Number(AUTO)),
    ("column-fill", ValueKind::Other),
    ("column-gap", ValueKind::Length(NORMAL)),
    ("column-rule", ValueKind::Other),
    ("column-rule-color", ValueKind::Color(COLOR)),
    ("column-rule-style", ValueKind::Other),
    ("column-rule-width", ValueKind::Length(BORDER_WIDTH)),
    ("column-span", ValueKind::Other),
    ("column-width", ValueKind::Length(AUTO)),
    ("columns", ValueKind::Other),
    ("contain", ValueKind::Other),
    ("contain-intrinsic-block-size", ValueKind::Other),
    ("contain-intrinsic-height", ValueKind::Other),
    ("contain-intrinsic-inline-size", ValueKind::Other),
    ("contain-intrinsic-size", ValueKind::Other),
    ("contain-intrinsic-width", ValueKind::Other),
    ("container", ValueKind::Other),
    ("container-name", ValueKind::Other),
    ("container-type", ValueKind::Other),
    ("content", ValueKind::Other),
    ("content-visibility", ValueKind::Other),
    ("counter-increment", ValueKind::Other),
    ("counter-reset", ValueKind::Other),
    ("counter-set", ValueKind::Other),
    ("cursor", ValueKind::Other),
    ("cx", ValueKind::Other),
    ("cy", ValueKind::Other),
    ("d", ValueKind::Other),
    ("direction", ValueKind::Keywords(&["ltr", "rtl"])),
    ("display", ValueKind::Keywords(DISPLAY)),
    ("dominant-baseline", ValueKind::Other),
    ("empty-cells", ValueKind::Other),
    ("field-sizing", ValueKind::Other),
    ("fill", ValueKind::Other),
    ("fill-opacity", ValueKind::Other),
    ("fill-rule", ValueKind::Other),
    ("filter", ValueKind::Other),
    ("flex", ValueKind::Dedicated),
    (
        "flex-basis",
        ValueKind::Length(&[
            "auto",
            "content",
            "min-content",
            "max-content",
            "fit-content",
        ]),
    ),
    (
        "flex-direction",
        ValueKind::Keywords(&["row", "row-reverse", "column", "column-reverse"]),
    ),
    ("flex-flow", ValueKind::Other),
    ("flex-grow", ValueKind::Number(NO_KEYWORDS)),
    ("flex-shrink", ValueKind::Number(NO_KEYWORDS)),
    (
        "flex-wrap",
        ValueKind::Keywords(&["nowrap", "wrap", "wrap-reverse"]),
    ),
    (
        "float",
        ValueKind::Keywords(&["none", "left", "right", "inline-start", "inline-end"]),
    ),
    ("flood-color", ValueKind::Other),
    ("flood-opacity", ValueKind::Other),
    ("font", ValueKind::Other),
    ("font-display", ValueKind::Other),
    ("font-family", ValueKind::Dedicated),
    ("font-feature-settings", ValueKind::Other),
    ("font-kerning", ValueKind::Other),
    ("font-optical-sizing", ValueKind::Other),
    ("font-palette", ValueKind::Other),
    ("font-size", ValueKind::Length(FONT_SIZE)),
    ("font-size-adjust", ValueKind::Other),
    ("font-stretch", ValueKind::Other),
    (
        "font-style",
        ValueKind::Keywords(&["normal", "italic", "oblique"]),
    ),
    ("font-synthesis", ValueKind::Other),
    ("font-synthesis-small-caps", ValueKind::Other),
    ("font-synthesis-style", ValueKind::Other),
    ("font-synthesis-weight", ValueKind::Other),
    ("font-variant", ValueKind::Other),
    ("font-variant-alternates", ValueKind::Other),
    ("font-variant-caps", ValueKind::Other),
    ("font-variant-east-asian", ValueKind::Other),
    ("font-variant-emoji", ValueKind::Other),
    ("font-variant-ligatures", ValueKind::Other),
    ("font-variant-numeric", ValueKind::Other),
    ("font-variant-position", ValueKind::Other),
    ("font-variation-settings", ValueKind::Other),
    ("font-weight", ValueKind::Number(FONT_WEIGHT)),
    ("forced-color-adjust", ValueKind::Other),
    ("gap", ValueKind::Length(NORMAL)),
    ("grid", ValueKind::Other),
    ("grid-area", ValueKind::Other),
    ("grid-auto-columns", ValueKind::Other),
    (
        "grid-auto-flow",
        ValueKind::Keywords(&["row", "column", "dense"]),
    ),
    ("grid-auto-rows", ValueKind::Other),
    ("grid-column", ValueKind::Dedicated),
    ("grid-column-end", ValueKind::Other),
    ("grid-column-gap", ValueKind::Length(NORMAL)),
    ("grid-column-start", ValueKind::Other),
    ("grid-gap", ValueKind::Length(NORMAL)),
    ("grid-row", ValueKind::Dedicated),
    ("grid-row-end", ValueKind::Other),
    ("grid-row-gap", ValueKind::Length(NORMAL)),
    ("grid-row-start", ValueKind::Other),
    ("grid-template", ValueKind::Dedicated),
    ("grid-template-areas", ValueKind::Dedicated),
    ("grid-template-columns", ValueKind::Dedicated),
    ("grid-template-rows", ValueKind::Dedicated),
    ("hanging-punctuation", ValueKind::Other),
    ("height", ValueKind::Length(SIZE)),
    ("hyphenate-character", ValueKind::Other),
    ("hyphenate-limit-chars", ValueKind::Other),
    ("hyphens", ValueKind::Other),
    ("image-orientation", ValueKind::Other),
    ("image-rendering", ValueKind::Other),
    ("initial-letter", ValueKind::Other),
    ("inline-size", ValueKind::Length(SIZE)),
    ("inset", ValueKind::Dedicated),
    ("inset-block", ValueKind::Other),
    ("inset-block-end", ValueKind::Length(AUTO)),
    ("inset-block-start", ValueKind::Length(AUTO)),
    ("inset-inline", ValueKind::Other),
    ("inset-inline-end", ValueKind::Length(AUTO)),
    ("inset-inline-start", ValueKind::Length(AUTO)),
    ("interpolate-size", ValueKind::Other),
    ("isolation", ValueKind::Keywords(&["auto", "isolate"])),
    ("justify-content", ValueKind::Keywords(CONTENT_ALIGN)),
    ("justify-items", ValueKind::Keywords(ITEMS_ALIGN)),
    ("justify-self", ValueKind::Keywords(SELF_ALIGN)),
    ("left", ValueKind::Length(AUTO)),
    ("letter-spacing", ValueKind::Length(NORMAL)),
    ("lighting-color", ValueKind::Other),
    ("line-break", ValueKind::Other),
    ("line-clamp", ValueKind::Other),
    ("line-height", ValueKind::Length(NORMAL)),
    ("list-style", ValueKind::Other),
    ("list-style-image", ValueKind::Image(NONE)),
    (
        "list-style-position",
        ValueKind::Keywords(&["inside", "outside"]),
    ),
    ("list-style-type", ValueKind::Other),
    ("margin", ValueKind::Dedicated),
    ("margin-block", ValueKind::Other),
    ("margin-block-end", ValueKind::Length(AUTO)),
    ("margin-block-start", ValueKind::Length(AUTO)),
    ("margin-bottom", ValueKind::Length(AUTO)),
    ("margin-inline", ValueKind::Other),
    ("margin-inline-end", ValueKind::Length(AUTO)),
    ("margin-inline-start", ValueKind::Length(AUTO)),
    ("margin-left", ValueKind::Length(AUTO)),
    ("margin-right", ValueKind::Length(AUTO)),
    ("margin-top", ValueKind::Length(AUTO)),
    ("marker", ValueKind::Other),
    ("marker-end", ValueKind::Other),
    ("marker-mid", ValueKind::Other),
    ("marker-start", ValueKind::Other),
    ("mask", ValueKind::Other),
    ("mask-clip", ValueKind::Other),
    ("mask-composite", ValueKind::Other),
    ("mask-image", ValueKind::Other),
    ("mask-mode", ValueKind::Other),
    ("mask-origin", ValueKind::Other),
    ("mask-position", ValueKind::Other),
    ("mask-repeat", ValueKind::Other),
    ("mask-size", ValueKind::Other),
    ("mask-type", ValueKind::Other),
    ("math-depth", ValueKind::Other),
    ("math-shift", ValueKind::Other),
    ("math-style", ValueKind::Other),
    ("max-block-size", ValueKind::Length(MAX_SIZE)),
    ("max-height", ValueKind::Length(MAX_SIZE)),
    ("max-inline-size", ValueKind::Length(MAX_SIZE)),
    ("max-width", ValueKind::Length(MAX_SIZE)),
    ("min-block-size", ValueKind::Length(SIZE)),
    ("min-height", ValueKind::Length(SIZE)),
    ("min-inline-size", ValueKind::Length(SIZE)),
    ("min-width", ValueKind::Length(SIZE)),
    ("mix-blend-mode", ValueKind::Other),
    (
        "object-fit",
        ValueKind::Keywords(&["fill", "contain", "cover", "none", "scale-down"]),
    ),
    ("object-position", ValueKind::Other),
    ("object-view-box", ValueKind::Other),
    ("offset", ValueKind::Other),
    ("offset-anchor", ValueKind::Other),
    ("offset-distance", ValueKind::Other),
    ("offset-path", ValueKind::Other),
    ("offset-position", ValueKind::Other),
    ("offset-rotate", ValueKind::Other),
    ("opacity", ValueKind::Number(NO_KEYWORDS)),
    ("order", ValueKind::Number(NO_KEYWORDS)),
    ("orphans", ValueKind::Number(NO_KEYWORDS)),
    ("outline", ValueKind::Other),
    (
        "outline-color",
        ValueKind::Color(&["auto", "currentcolor", "transparent"]),
    ),
    ("outline-offset", ValueKind::Length(NO_KEYWORDS)),
    ("outline-style", ValueKind::Other),
    ("outline-width", ValueKind::Length(BORDER_WIDTH)),
    ("overflow", ValueKind::Keywords(OVERFLOW)),
    ("overflow-anchor", ValueKind::Other),
    ("overflow-block", ValueKind::Keywords(OVERFLOW)),
    ("overflow-clip-margin", ValueKind::Other),
    ("overflow-inline", ValueKind::Keywords(OVERFLOW)),
    (
        "overflow-wrap",
        ValueKind::Keywords(&["normal", "break-word", "anywhere"]),
    ),
    ("overflow-x", ValueKind::Keywords(OVERFLOW)),
    ("overflow-y", ValueKind::Keywords(OVERFLOW)),
    ("overlay", ValueKind::Other),
    ("overscroll-behavior", ValueKind::Other),
    ("overscroll-behavior-block", ValueKind::Other),
    ("overscroll-behavior-inline", ValueKind::Other),
    ("overscroll-behavior-x", ValueKind::Other),
    ("overscroll-behavior-y", ValueKind::Other),
    ("padding", ValueKind::Dedicated),
    ("padding-block", ValueKind::Other),
    ("padding-block-end", ValueKind::Length(NO_KEYWORDS)),
    ("padding-block-start", ValueKind::Length(NO_KEYWORDS)),
    ("padding-bottom", ValueKind::Length(NO_KEYWORDS)),
    ("padding-inline", ValueKind::Other),
    ("padding-inline-end", ValueKind::Length(NO_KEYWORDS)),
    ("padding-inline-start", ValueKind::Length(NO_KEYWORDS)),
    ("padding-left", ValueKind::Length(NO_KEYWORDS)),
    ("padding-right", ValueKind::Length(NO_KEYWORDS)),
    ("padding-top", ValueKind::Length(NO_KEYWORDS)),
    ("page", ValueKind::Other),
    ("page-break-after", ValueKind::Other),
    ("page-break-before", ValueKind::Other),
    ("page-break-inside", ValueKind::Other),
    ("paint-order", ValueKind::Other),
    ("perspective", ValueKind::Length(NONE)),
    ("perspective-origin", ValueKind::Other),
    ("place-content", ValueKind::Other),
    ("place-items", ValueKind::Other),
    ("place-self", ValueKind::Other),
    ("pointer-events", ValueKind::Other),
    (
        "position",
        ValueKind::Keywords(&["static", "relative", "absolute", "fixed", "sticky"]),
    ),
    ("position-anchor", ValueKind::Other),
    ("position-area", ValueKind::Other),
    ("position-try", ValueKind::Other),
    ("position-try-fallbacks", ValueKind::Other),
    ("position-try-order", ValueKind::Other),
    ("position-visibility", ValueKind::Other),
    ("print-color-adjust", ValueKind::Other),
    ("quotes", ValueKind::Other),
    ("r", ValueKind::Other),
    ("reading-flow", ValueKind::Other),
    ("resize", ValueKind::Other),
    ("right", ValueKind::Length(AUTO)),
    ("rotate", ValueKind::Other),
    ("row-gap", ValueKind::Length(NORMAL)),
    ("ruby-align", ValueKind::Other),
    ("ruby-position", ValueKind::Other),
    ("rx", ValueKind::Other),
    ("ry", ValueKind::Other),
    ("scale", ValueKind::Other),
    ("scroll-behavior", ValueKind::Keywords(&["auto", "smooth"])),
    ("scroll-margin", ValueKind::Other),
    ("scroll-margin-block", ValueKind::Other),
    ("scroll-margin-block-end", ValueKind::Other),
    ("scroll-margin-block-start", ValueKind::Other),
    ("scroll-margin-bottom", ValueKind::Other),
    ("scroll-margin-inline", ValueKind::Other),
    ("scroll-margin-inline-end", ValueKind::Other),
    ("scroll-margin-inline-start", ValueKind::Other),
    ("scroll-margin-left", ValueKind::Other),
    ("scroll-margin-right", ValueKind::Other),
    ("scroll-margin-top", ValueKind::Other),
    ("scroll-marker-group", ValueKind::Other),
    ("scroll-padding", ValueKind::Other),
    ("scroll-padding-block", ValueKind::Other),
    ("scroll-padding-block-end", ValueKind::Other),
    ("scroll-padding-block-start", ValueKind::Other),
    ("scroll-padding-bottom", ValueKind::Other),
    ("scroll-padding-inline", ValueKind::Other),
    ("scroll-padding-inline-end", ValueKind::Other),
    ("scroll-padding-inline-start", ValueKind::Other),
    ("scroll-padding-left", ValueKind::Other),
    ("scroll-padding-right", ValueKind::Other),
    ("scroll-padding-top", ValueKind::Other),
    ("scroll-snap-align", ValueKind::Other),
    ("scroll-snap-stop", ValueKind::Other),
    ("scroll-snap-type", ValueKind::Other),
    ("scroll-timeline", ValueKind::Other),
    ("scroll-timeline-axis", ValueKind::Other),
    ("scroll-timeline-name", ValueKind::Other),
    ("scrollbar-color", ValueKind::Other),
    ("scrollbar-gutter", ValueKind::Other),
    (
        "scrollbar-width",
        ValueKind::Keywords(&["auto", "thin", "none"]),
    ),
    ("shape-image-threshold", ValueKind::Other),
    ("shape-margin", ValueKind::Other),
    ("shape-outside", ValueKind::Other),
    ("shape-rendering", ValueKind::Other),
    ("speak", ValueKind::Other),
    ("stop-color", ValueKind::Other),
    ("stop-opacity", ValueKind::Other),
    ("stroke", ValueKind::Other),
    ("stroke-dasharray", ValueKind::Other),
    ("stroke-dashoffset", ValueKind::Other),
    ("stroke-linecap", ValueKind::Other),
    ("stroke-linejoin", ValueKind::Other),
    ("stroke-miterlimit", ValueKind::Other),
    ("stroke-opacity", ValueKind::Other),
    ("stroke-width", ValueKind::Other),
    ("tab-size", ValueKind::Other),
    ("table-layout", ValueKind::Keywords(&["auto", "fixed"])),
    ("text-align", ValueKind::Keywords(TEXT_ALIGN)),
    ("text-align-last", ValueKind::Other),
    ("text-anchor", ValueKind::Other),
    ("text-autospace", ValueKind::Other),
    ("text-box", ValueKind::Other),
    ("text-box-edge", ValueKind::Other),
    ("text-box-trim", ValueKind::Other),
    ("text-combine-upright", ValueKind::Other),
    (
        "text-decoration",
        ValueKind::Keywords(&["none", "underline", "overline", "line-through"]),
    ),
    ("text-decoration-color", ValueKind::Color(COLOR)),
    ("text-decoration-line", ValueKind::Other),
    ("text-decoration-skip-ink", ValueKind::Other),
    ("text-decoration-style", ValueKind::Other),
    ("text-decoration-thickness", ValueKind::Other),
    ("text-emphasis", ValueKind::Other),
    ("text-emphasis-color", ValueKind::Other),
    ("text-emphasis-position", ValueKind::Other),
    ("text-emphasis-style", ValueKind::Other),
    ("text-indent", ValueKind::Length(NO_KEYWORDS)),
    ("text-orientation", ValueKind::Other),
    ("text-overflow", ValueKind::Keywords(&["clip", "ellipsis"])),
    ("text-rendering", ValueKind::Other),
    ("text-shadow", ValueKind::Dedicated),
    ("text-size-adjust", ValueKind::Other),
    ("text-spacing-trim", ValueKind::Other),
    (
        "text-transform",
        ValueKind::Keywords(&[
            "none",
            "capitalize",
            "uppercase",
            "lowercase",
            "full-width",
            "full-size-kana",
        ]),
    ),
    ("text-underline-offset", ValueKind::Other),
    ("text-underline-position", ValueKind::Other),
    ("text-wrap", ValueKind::Other),
    ("text-wrap-mode", ValueKind::Other),
    ("text-wrap-style", ValueKind::Other),
    ("timeline-scope", ValueKind::Other),
    ("top", ValueKind::Length(AUTO)),
    ("touch-action", ValueKind::Other),
    ("transform", ValueKind::Dedicated),
    ("transform-box", ValueKind::Other),
    ("transform-origin", ValueKind::Other),
    ("transform-style", ValueKind::Other),
    ("transition", ValueKind::Other),
    ("transition-behavior", ValueKind::Other),
    ("transition-delay", ValueKind::Other),
    ("transition-duration", ValueKind::Other),
    ("transition-property", ValueKind::Other),
    ("transition-timing-function", ValueKind::Other),
    ("translate", ValueKind::Other),
    ("unicode-bidi", ValueKind::Other),
    ("unicode-range", ValueKind::Other),
    ("user-select", ValueKind::Other),
    ("vector-effect", ValueKind::Other),
    ("vertical-align", ValueKind::Length(VERTICAL_ALIGN)),
    ("view-timeline", ValueKind::Other),
    ("view-timeline-axis", ValueKind::Other),
    ("view-timeline-inset", ValueKind::Other),
    ("view-timeline-name", ValueKind::Other),
    ("view-transition-class", ValueKind::Other),
    ("view-transition-name", ValueKind::Other),
    (
        "visibility",
        ValueKind::Keywords(&["visible", "hidden", "collapse"]),
    ),
    ("white-space", ValueKind::Keywords(WHITE_SPACE)),
    ("white-space-collapse", ValueKind::Other),
    ("widows", ValueKind::Number(NO_KEYWORDS)),
    ("width", ValueKind::Length(SIZE)),
    ("will-change", ValueKind::Other),
    (
        "word-break",
        ValueKind::Keywords(&[
            "normal",
            "break-all",
            "keep-all",
            "break-word",
            "auto-phrase",
        ]),
    ),
    ("word-spacing", ValueKind::Length(NORMAL)),
    (
        "word-wrap",
        ValueKind::Keywords(&["normal", "break-word", "anywhere"]),
    ),
    ("writing-mode", ValueKind::Other),
    ("x", ValueKind::Other),
    ("y", ValueKind::Other),
    ("z-index", ValueKind::Number(AUTO)),
    ("zoom", ValueKind::Other),
];

/// `-webkit-` properties Chrome accepts that have no standard twin in
/// [`PROPERTIES`] (or whose standard twin has a different name).
const WEBKIT_ONLY: &[&str] = &[
    "-webkit-border-horizontal-spacing",
    "-webkit-border-vertical-spacing",
    "-webkit-box-align",
    "-webkit-box-decoration-break",
    "-webkit-box-direction",
    "-webkit-box-flex",
    "-webkit-box-ordinal-group",
    "-webkit-box-orient",
    "-webkit-box-pack",
    "-webkit-box-reflect",
    "-webkit-font-smoothing",
    "-webkit-highlight",
    "-webkit-line-break",
    "-webkit-locale",
    "-webkit-mask-box-image",
    "-webkit-print-color-adjust",
    "-webkit-rtl-ordering",
    "-webkit-tap-highlight-color",
    "-webkit-text-combine",
    "-webkit-text-decorations-in-effect",
    "-webkit-text-fill-color",
    "-webkit-text-security",
    "-webkit-text-stroke",
    "-webkit-text-stroke-color",
    "-webkit-text-stroke-width",
    "-webkit-user-drag",
    "-webkit-user-modify",
];

/// The value kind of a property Chrome knows; `None` for an unknown name
/// (a typo, another engine's prefix, an IE hack). Custom properties
/// (`--*`) are known and take anything. A `-webkit-` alias of a listed
/// property gets that property's kind.
pub fn lookup(name: &str) -> Option<ValueKind> {
    let name = name.to_ascii_lowercase();
    if name.starts_with("--") {
        return Some(ValueKind::Other);
    }
    if let Some(kind) = find(&name) {
        return Some(kind);
    }
    if let Some(standard) = name.strip_prefix("-webkit-") {
        if WEBKIT_ONLY.contains(&name.as_str()) {
            return Some(ValueKind::Other);
        }
        return find(standard);
    }
    None
}

/// Properties something downstream of the cascade actually reads (layout,
/// paint, the agent index), sorted. A parsed declaration outside this list
/// is accepted and then ignored — the compat probe counts those as
/// "parse-only", and `@supports` (step 2.3) holds them unsupported. Kept by
/// hand from `styled.value("…")` call sites (inherited lookups included):
/// when the engine starts reading a property, add it here.
pub const RENDERED: &[&str] = &[
    "align-content",
    "align-items",
    "align-self",
    "all",
    "background-color",
    "background-image",
    "background-position-x",
    "background-position-y",
    "background-size",
    "border-bottom",
    "border-bottom-color",
    "border-bottom-left-radius",
    "border-bottom-right-radius",
    "border-bottom-style",
    "border-color",
    "border-left",
    "border-left-color",
    "border-left-style",
    "border-right",
    "border-right-color",
    "border-right-style",
    "border-spacing",
    "border-style",
    "border-top",
    "border-top-color",
    "border-top-left-radius",
    "border-top-right-radius",
    "border-top-style",
    "border-width",
    "bottom",
    "box-shadow",
    "box-sizing",
    "clear",
    "color",
    "column-gap",
    "display",
    "flex-basis",
    "flex-direction",
    "flex-grow",
    "flex-shrink",
    "flex-wrap",
    "float",
    "font-family",
    "font-size",
    "font-weight",
    "gap",
    "grid-area",
    "grid-auto-flow",
    "grid-column",
    "grid-row",
    "grid-template-areas",
    "grid-template-columns",
    "grid-template-rows",
    "height",
    "justify-content",
    "left",
    "line-height",
    "margin-bottom",
    "margin-left",
    "margin-right",
    "margin-top",
    "max-height",
    "max-width",
    "min-height",
    "min-width",
    "opacity",
    "overflow",
    "overflow-x",
    "overflow-y",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "padding-top",
    "position",
    "right",
    "row-gap",
    "text-align",
    "text-decoration",
    "text-shadow",
    "top",
    "transform",
    "vertical-align",
    "white-space",
    "width",
    "z-index",
];

/// Whether the engine reads `name` (lower-cased) after the cascade.
pub fn is_rendered(name: &str) -> bool {
    RENDERED.binary_search(&name).is_ok()
}

/// The `-webkit-` names Chrome accepts as aliases of a standard property
/// (its `css_properties.json5` `alias_for` list, sorted; follow-up F2). The
/// parser stores a declaration under the standard name and parses it with
/// that property's parser. Other engines' prefixes (`-moz-`, `-ms-`) are
/// never aliases in Chrome. [`lookup`] still accepts any `-webkit-` form of
/// a listed property — the frozen ours / browser-too table — but only
/// these are routed.
pub const WEBKIT_ALIASES: &[(&str, &str)] = &[
    ("-webkit-align-content", "align-content"),
    ("-webkit-align-items", "align-items"),
    ("-webkit-align-self", "align-self"),
    ("-webkit-animation", "animation"),
    ("-webkit-animation-delay", "animation-delay"),
    ("-webkit-animation-direction", "animation-direction"),
    ("-webkit-animation-duration", "animation-duration"),
    ("-webkit-animation-fill-mode", "animation-fill-mode"),
    (
        "-webkit-animation-iteration-count",
        "animation-iteration-count",
    ),
    ("-webkit-animation-name", "animation-name"),
    ("-webkit-animation-play-state", "animation-play-state"),
    (
        "-webkit-animation-timing-function",
        "animation-timing-function",
    ),
    ("-webkit-appearance", "appearance"),
    ("-webkit-backdrop-filter", "backdrop-filter"),
    ("-webkit-backface-visibility", "backface-visibility"),
    ("-webkit-background-clip", "background-clip"),
    ("-webkit-background-origin", "background-origin"),
    ("-webkit-background-size", "background-size"),
    ("-webkit-border-after", "border-block-end"),
    ("-webkit-border-after-color", "border-block-end-color"),
    ("-webkit-border-after-style", "border-block-end-style"),
    ("-webkit-border-after-width", "border-block-end-width"),
    ("-webkit-border-before", "border-block-start"),
    ("-webkit-border-before-color", "border-block-start-color"),
    ("-webkit-border-before-style", "border-block-start-style"),
    ("-webkit-border-before-width", "border-block-start-width"),
    (
        "-webkit-border-bottom-left-radius",
        "border-bottom-left-radius",
    ),
    (
        "-webkit-border-bottom-right-radius",
        "border-bottom-right-radius",
    ),
    ("-webkit-border-end", "border-inline-end"),
    ("-webkit-border-end-color", "border-inline-end-color"),
    ("-webkit-border-end-style", "border-inline-end-style"),
    ("-webkit-border-end-width", "border-inline-end-width"),
    ("-webkit-border-radius", "border-radius"),
    ("-webkit-border-start", "border-inline-start"),
    ("-webkit-border-start-color", "border-inline-start-color"),
    ("-webkit-border-start-style", "border-inline-start-style"),
    ("-webkit-border-start-width", "border-inline-start-width"),
    ("-webkit-border-top-left-radius", "border-top-left-radius"),
    ("-webkit-border-top-right-radius", "border-top-right-radius"),
    ("-webkit-box-shadow", "box-shadow"),
    ("-webkit-box-sizing", "box-sizing"),
    ("-webkit-clip-path", "clip-path"),
    ("-webkit-column-count", "column-count"),
    ("-webkit-column-gap", "column-gap"),
    ("-webkit-column-rule", "column-rule"),
    ("-webkit-column-rule-color", "column-rule-color"),
    ("-webkit-column-rule-style", "column-rule-style"),
    ("-webkit-column-rule-width", "column-rule-width"),
    ("-webkit-column-span", "column-span"),
    ("-webkit-column-width", "column-width"),
    ("-webkit-columns", "columns"),
    ("-webkit-filter", "filter"),
    ("-webkit-flex", "flex"),
    ("-webkit-flex-basis", "flex-basis"),
    ("-webkit-flex-direction", "flex-direction"),
    ("-webkit-flex-flow", "flex-flow"),
    ("-webkit-flex-grow", "flex-grow"),
    ("-webkit-flex-shrink", "flex-shrink"),
    ("-webkit-flex-wrap", "flex-wrap"),
    ("-webkit-font-feature-settings", "font-feature-settings"),
    ("-webkit-hyphens", "hyphens"),
    ("-webkit-justify-content", "justify-content"),
    ("-webkit-logical-height", "block-size"),
    ("-webkit-logical-width", "inline-size"),
    ("-webkit-margin-after", "margin-block-end"),
    ("-webkit-margin-before", "margin-block-start"),
    ("-webkit-margin-end", "margin-inline-end"),
    ("-webkit-margin-start", "margin-inline-start"),
    ("-webkit-mask", "mask"),
    ("-webkit-mask-clip", "mask-clip"),
    ("-webkit-mask-composite", "mask-composite"),
    ("-webkit-mask-image", "mask-image"),
    ("-webkit-mask-origin", "mask-origin"),
    ("-webkit-mask-position", "mask-position"),
    ("-webkit-mask-repeat", "mask-repeat"),
    ("-webkit-mask-size", "mask-size"),
    ("-webkit-max-logical-height", "max-block-size"),
    ("-webkit-max-logical-width", "max-inline-size"),
    ("-webkit-min-logical-height", "min-block-size"),
    ("-webkit-min-logical-width", "min-inline-size"),
    ("-webkit-opacity", "opacity"),
    ("-webkit-order", "order"),
    ("-webkit-padding-after", "padding-block-end"),
    ("-webkit-padding-before", "padding-block-start"),
    ("-webkit-padding-end", "padding-inline-end"),
    ("-webkit-padding-start", "padding-inline-start"),
    ("-webkit-perspective", "perspective"),
    ("-webkit-perspective-origin", "perspective-origin"),
    ("-webkit-shape-image-threshold", "shape-image-threshold"),
    ("-webkit-shape-margin", "shape-margin"),
    ("-webkit-shape-outside", "shape-outside"),
    ("-webkit-text-emphasis", "text-emphasis"),
    ("-webkit-text-emphasis-color", "text-emphasis-color"),
    ("-webkit-text-emphasis-position", "text-emphasis-position"),
    ("-webkit-text-emphasis-style", "text-emphasis-style"),
    ("-webkit-text-size-adjust", "text-size-adjust"),
    ("-webkit-transform", "transform"),
    ("-webkit-transform-origin", "transform-origin"),
    ("-webkit-transform-style", "transform-style"),
    ("-webkit-transition", "transition"),
    ("-webkit-transition-delay", "transition-delay"),
    ("-webkit-transition-duration", "transition-duration"),
    ("-webkit-transition-property", "transition-property"),
    (
        "-webkit-transition-timing-function",
        "transition-timing-function",
    ),
    ("-webkit-user-select", "user-select"),
    ("-webkit-writing-mode", "writing-mode"),
];

/// The standard property a Chrome `-webkit-` alias stands for (`None` for
/// any other name). Expects a lower-cased name.
pub fn chrome_alias(name: &str) -> Option<&'static str> {
    WEBKIT_ALIASES
        .binary_search_by(|(alias, _)| (*alias).cmp(name))
        .ok()
        .map(|index| WEBKIT_ALIASES[index].1)
}

/// The standard name of a Chrome `-webkit-` alias (`-webkit-transform` →
/// `transform`); any other name as given. Expects a lower-cased name.
pub fn unprefixed(name: &str) -> &str {
    chrome_alias(name).unwrap_or(name)
}

fn find(name: &str) -> Option<ValueKind> {
    PROPERTIES
        .binary_search_by(|(entry, _)| (*entry).cmp(name))
        .ok()
        .map(|index| PROPERTIES[index].1)
}

/// Whether `name` carries another engine's vendor prefix — those
/// declarations (and selectors) are dropped by Chrome as well.
pub fn is_foreign_vendor_prefix(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["-moz-", "-ms-", "-o-", "-khtml-"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::{
        PROPERTIES, RENDERED, ValueKind, WEBKIT_ALIASES, WEBKIT_ONLY, is_foreign_vendor_prefix,
        lookup,
    };

    #[test]
    fn table_is_sorted_and_unique_for_binary_search() {
        for pair in PROPERTIES.windows(2) {
            assert!(pair[0].0 < pair[1].0, "{} !< {}", pair[0].0, pair[1].0);
        }
        for name in WEBKIT_ONLY {
            assert!(name.starts_with("-webkit-"));
        }
        for pair in RENDERED.windows(2) {
            assert!(pair[0] < pair[1], "{} !< {}", pair[0], pair[1]);
        }
        assert!(RENDERED.iter().all(|name| lookup(name).is_some()));
        for pair in WEBKIT_ALIASES.windows(2) {
            assert!(pair[0].0 < pair[1].0, "{} !< {}", pair[0].0, pair[1].0);
        }
        for (alias, standard) in WEBKIT_ALIASES {
            assert!(alias.starts_with("-webkit-"), "{alias}");
            assert!(lookup(standard).is_some(), "{alias} → unknown {standard}");
        }
    }

    #[test]
    fn lookup_knows_standard_custom_and_webkit_aliases() {
        assert_eq!(lookup("display").map(ValueKind::is_typed), Some(true));
        assert_eq!(lookup("Display"), lookup("display"));
        assert_eq!(lookup("scroll-snap-type"), Some(ValueKind::Other));
        assert_eq!(lookup("--brand-color"), Some(ValueKind::Other));
        assert_eq!(lookup("-webkit-transform"), lookup("transform"));
        assert_eq!(lookup("-webkit-appearance"), Some(ValueKind::Other));
        assert_eq!(lookup("-webkit-font-smoothing"), Some(ValueKind::Other));
        assert_eq!(lookup("-webkit-frobnicate"), None);
        assert_eq!(lookup("colour"), None);
        assert_eq!(lookup("*zoom"), None);
        assert_eq!(lookup("-moz-osx-font-smoothing"), None);
    }

    #[test]
    fn keyword_acceptance_is_per_kind() {
        let width = lookup("width").unwrap();
        assert!(width.accepts_keyword("auto"));
        assert!(width.accepts_keyword("INHERIT"));
        assert!(!width.accepts_keyword("100vw"));
        assert!(lookup("color").unwrap().accepts_keyword("currentColor"));
        assert!(!lookup("display").unwrap().accepts_keyword("flexbox"));
        assert!(!lookup("cursor").unwrap().is_typed());
    }

    #[test]
    fn foreign_prefixes_are_not_webkit() {
        assert!(is_foreign_vendor_prefix("-moz-appearance"));
        assert!(is_foreign_vendor_prefix("-MS-filter"));
        assert!(!is_foreign_vendor_prefix("-webkit-appearance"));
        assert!(!is_foreign_vendor_prefix("--moz-var"));
    }
}
