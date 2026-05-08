use std::collections::{BTreeMap, HashMap, HashSet};

use selectors::context::{
    MatchingContext, MatchingForInvalidation, MatchingMode, NeedsSelectorFlags, QuirksMode,
    SelectorCaches,
};

use crate::{
    css::{Declaration, Selector, Stylesheet, TrackSize, Unit, Value},
    dom::{Document, ElementData, NodeId, NodeType},
    dom_select::{MatchingElement, MatchingState},
};

pub type PropertyMap = BTreeMap<String, Value>;

// Mirrors a real browser's user-agent default for fonts. Used as the baseline whenever
// no font-size is in scope (root with no font-size declaration, em/rem on the root, etc.).
const DEFAULT_FONT_SIZE: f32 = 16.0;

// StyledNode mirrors the DOM tree but replaces raw attributes with resolved CSS properties.
//
// `node_id` is the back-reference into the arena that produced this tree —
// useful when callers (hit-testing, future mutation observers) need to find
// the underlying DOM node again. `node_type` is a snapshot taken at style
// time so layout/render can read tag and text without threading a `&Document`
// borrow through every helper. The snapshot is correct because style is
// always re-run against the current Document; layout/render never observe a
// post-mutation tree without a fresh styling pass.
#[derive(Debug, Clone, PartialEq)]
pub struct StyledNode {
    pub node_id: NodeId,
    pub node_type: NodeType,
    pub specified_values: PropertyMap,
    pub children: Vec<StyledNode>,
}

impl StyledNode {
    pub fn value(&self, name: &str) -> Option<&Value> {
        self.specified_values.get(name)
    }
}

/// User-facing interaction state passed into `style_tree_with_state`. Each field is the
/// DOM path (sequence of child indices from the root) of the corresponding node, or
/// `None` if the state is not active.
#[derive(Default, Copy, Clone, Debug)]
pub struct InteractionState<'a> {
    pub hover: Option<&'a [usize]>,
    pub focus: Option<&'a [usize]>,
    pub active: Option<&'a [usize]>,
}

pub fn style_tree(document: &Document, root: NodeId, stylesheets: &[Stylesheet]) -> StyledNode {
    // Most callers do not care about interaction state — they get a "nothing engaged" tree.
    style_tree_with_state(document, root, stylesheets, InteractionState::default())
}

/// Backward-compatible convenience: same as `style_tree_with_state` with only the
/// hovered path filled in. New callers should reach for `style_tree_with_state` directly.
pub fn style_tree_with_hover(
    document: &Document,
    root: NodeId,
    stylesheets: &[Stylesheet],
    hovered_path: Option<&[usize]>,
) -> StyledNode {
    style_tree_with_state(
        document,
        root,
        stylesheets,
        InteractionState {
            hover: hovered_path,
            ..Default::default()
        },
    )
}

/// Build the styled tree given a complete picture of interaction state. Each path slice
/// identifies the node under that interaction; matching uses CSS-spec semantics —
/// :hover and :active propagate to ancestors of the deepest engaged node, while :focus
/// only matches the focused node itself.
pub fn style_tree_with_state(
    document: &Document,
    root: NodeId,
    stylesheets: &[Stylesheet],
    state: InteractionState<'_>,
) -> StyledNode {
    // Resolve the user-supplied paths into NodeIds once. The matching layer
    // walks parent links from these NodeIds to compute :hover / :focus /
    // :active per element, which is cheaper than re-comparing path slices
    // for every selector evaluation.
    //
    // The original cascade interpreted the empty path `[]` as "the root
    // currently being styled" and `[0]` as that root's first child. We
    // mirror that here so `style_tree_with_hover(..., Some(&[0]))` keeps
    // its old meaning rather than the document-roots interpretation
    // `Document::resolve_path` exposes.
    let matching_state = MatchingState {
        hover: state.hover.and_then(|p| resolve_relative_path(document, root, p)),
        focus: state.focus.and_then(|p| resolve_relative_path(document, root, p)),
        active: state.active.and_then(|p| resolve_relative_path(document, root, p)),
    };
    // The root font-size feeds rem resolution for every descendant. Compute it up front
    // by treating the root as if it lived inside the user-agent default font-size.
    let raw_root = specified_values(document, root, stylesheets, &matching_state);
    let root_font_size = resolve_font_size(
        raw_root.get("font-size"),
        DEFAULT_FONT_SIZE,
        DEFAULT_FONT_SIZE,
    );
    style_tree_inner(
        document,
        root,
        stylesheets,
        None,
        root_font_size,
        &matching_state,
    )
}

/// Walk a `Vec<usize>` child-index path starting from `root` (rather than
/// the document's first root, which is what `Document::resolve_path`
/// does). Used to resolve the InteractionState paths the cascade is
/// handed by callers — the empty path means `root` itself, `[0]` means
/// `root`'s first child, etc.
fn resolve_relative_path(document: &Document, root: NodeId, path: &[usize]) -> Option<NodeId> {
    let mut current = root;
    for idx in path {
        let node = document.get(current)?;
        current = *node.children.get(*idx)?;
    }
    Some(current)
}

fn style_tree_inner(
    document: &Document,
    node_id: NodeId,
    stylesheets: &[Stylesheet],
    parent_values: Option<&PropertyMap>,
    root_font_size: f32,
    state: &MatchingState,
) -> StyledNode {
    let mut specified_values = specified_values(document, node_id, stylesheets, state);

    // Real browsers inherit many properties. Here we only inherit a few text-related ones
    // because they make documents readable without making the style system much more complex.
    for property in [
        "color",
        "font-size",
        "text-align",
        "line-height",
        "text-shadow",
        // white-space inherits per CSS spec; the layout/render whitespace
        // collapse helper consults this on the text node, which is always
        // a child of an element — without inheritance the text wouldn't
        // see `<pre>`'s `white-space: pre` declaration.
        "white-space",
        // Spec-mandated inheritance — without it, the UA defaults that put
        // `<pre>` / `<code>` / `<kbd>` / `<samp>` / `<tt>` into a monospace
        // family would never reach the text child the renderer actually
        // shapes, so the page would still draw code in the proportional
        // fallback font.
        "font-family",
    ] {
        if !specified_values.contains_key(property)
            && let Some(value) = parent_values.and_then(|values| values.get(property))
        {
            specified_values.insert(property.to_string(), value.clone());
        }
    }

    // CSS Custom Properties inherit per the CSS Variables spec. Pull every
    // `--*` declaration from the parent that isn't shadowed locally so this
    // node can resolve `var()` references against ancestor values too. The
    // var-resolve pass below sees the merged map.
    if let Some(parent) = parent_values {
        for (name, value) in parent {
            if name.starts_with("--") && !specified_values.contains_key(name) {
                specified_values.insert(name.clone(), value.clone());
            }
        }
    }

    // Resolve `var()` references against the `--*` declarations now in scope.
    // Done before the em/rem rewrite below so a custom property that holds
    // a length (e.g. `--gap: 1em`) goes through the same em/rem conversion
    // as if it had been written inline. Cycle-protected; unresolved
    // references with no fallback degrade to `Keyword("initial")`.
    resolve_var_references(&mut specified_values);

    // Font-size is resolved first because every other em-based length on this node depends
    // on it. Parent font-size has already been resolved to Px during the parent's pass, so
    // looking it up here is a straightforward read.
    let parent_font_size = parent_values
        .and_then(|values| values.get("font-size"))
        .and_then(|value| match value {
            Value::Length(v, Unit::Px) => Some(*v),
            _ => None,
        })
        .unwrap_or(DEFAULT_FONT_SIZE);
    let own_font_size = resolve_font_size(
        specified_values.get("font-size"),
        parent_font_size,
        root_font_size,
    );

    // Replace own font-size with the resolved Px value so descendants see the cascaded
    // value, then resolve every other em/rem/ch to Px in place. Percent stays
    // untouched — it depends on layout-time containing-block dimensions.
    //
    // `ch` is the advance width of "0" in the element's font; spec asks for a
    // glyph metric, but the proportional fonts pages tend to use for body
    // copy hover around `font-size * 0.5`, which is what Chrome falls back to
    // for fonts that don't expose the metric. That approximation is enough
    // for `max-width: 65ch` style reading-width sizing to land within ~5% of
    // the spec value, which is what 5.2 actually targets.
    specified_values.insert("font-size".into(), Value::Length(own_font_size, Unit::Px));
    let ch_unit = own_font_size * 0.5;
    // 1pt = 1/72in, CSS pins 1in = 96px → 4/3 px.
    const PT_TO_PX: f32 = 4.0 / 3.0;
    for value in specified_values.values_mut() {
        match value {
            Value::Length(v, Unit::Em) => *value = Value::Length(*v * own_font_size, Unit::Px),
            Value::Length(v, Unit::Rem) => *value = Value::Length(*v * root_font_size, Unit::Px),
            Value::Length(v, Unit::Ch) => *value = Value::Length(*v * ch_unit, Unit::Px),
            Value::Length(v, Unit::Pt) => *value = Value::Length(*v * PT_TO_PX, Unit::Px),
            // Track lists (grid-template-columns/rows) can mix length tracks
            // with fr tracks; resolve em/rem/ch/pt inside Length tracks the same
            // way we resolve top-level lengths so layout only ever sees Px / %.
            Value::TrackList(tracks) => {
                for track in tracks.iter_mut() {
                    match track {
                        TrackSize::Length(v, Unit::Em) => {
                            *track = TrackSize::Length(*v * own_font_size, Unit::Px);
                        }
                        TrackSize::Length(v, Unit::Rem) => {
                            *track = TrackSize::Length(*v * root_font_size, Unit::Px);
                        }
                        TrackSize::Length(v, Unit::Ch) => {
                            *track = TrackSize::Length(*v * ch_unit, Unit::Px);
                        }
                        TrackSize::Length(v, Unit::Pt) => {
                            *track = TrackSize::Length(*v * PT_TO_PX, Unit::Px);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // The selectors crate walks the live DOM via the `Element` trait
    // implementation in `dom_select.rs`, so we no longer need to thread an
    // explicit ancestor chain through the recursion.
    let node_data = document
        .get(node_id)
        .expect("style_tree_inner called with invalid NodeId");
    let children = node_data
        .children
        .iter()
        .map(|child_id| {
            style_tree_inner(
                document,
                *child_id,
                stylesheets,
                Some(&specified_values),
                root_font_size,
                state,
            )
        })
        .collect();

    StyledNode {
        node_id,
        node_type: node_data.node_type.clone(),
        specified_values,
        children,
    }
}

fn resolve_font_size(raw: Option<&Value>, parent_font_size: f32, root_font_size: f32) -> f32 {
    match raw {
        Some(Value::Length(v, Unit::Px)) => *v,
        Some(Value::Length(v, Unit::Em)) => *v * parent_font_size,
        Some(Value::Length(v, Unit::Rem)) => *v * root_font_size,
        // `font-size: 1ch` is unusual but legal; spec resolves the ch unit
        // against the parent's font (since the element's own font isn't
        // determined yet), so we use parent_font_size * 0.5 here — same
        // ratio the cascade applies to other ch lengths below, just rooted
        // one level up to break the chicken-and-egg.
        Some(Value::Length(v, Unit::Ch)) => *v * parent_font_size * 0.5,
        // 1pt = 4/3 px (1/72in × 96px/in). Common on legacy pages: HN sets
        // `font-size: 10pt` which lands at 13.33px — 25% larger than the bare
        // 10px we used to fall back to before pt was a first-class unit.
        Some(Value::Length(v, Unit::Pt)) => *v * (4.0 / 3.0),
        // CSS spec resolves font-size: <percent> against the parent's font-size, just like em.
        Some(Value::Length(v, Unit::Percent)) => *v / 100.0 * parent_font_size,
        _ => parent_font_size,
    }
}

fn specified_values(
    document: &Document,
    node_id: NodeId,
    stylesheets: &[Stylesheet],
    state: &MatchingState,
) -> PropertyMap {
    let mut matched = Vec::new();
    let element = MatchingElement::new(node_id, document, state);

    // The selectors crate's `MatchingContext` carries caches that get
    // mutated during a match (the nth-index cache, the relative-selector
    // cache, etc.). We rebuild it once per node — the caches grow over
    // the course of one styling pass which is exactly the reuse window
    // the design intends; recreating per-rule would erase that benefit.
    let mut caches = SelectorCaches::default();
    let mut ctx = MatchingContext::<MiniBrowserSelectorImplAlias>::new(
        MatchingMode::Normal,
        None,
        &mut caches,
        QuirksMode::NoQuirks,
        NeedsSelectorFlags::No,
        MatchingForInvalidation::No,
    );

    // First collect every rule that matches this node together with its
    // specificity and source order.
    for (rule_order, rule) in stylesheets
        .iter()
        .flat_map(|sheet| sheet.rules.iter())
        .enumerate()
    {
        if let Some(specificity) = matching_specificity(&element, &mut ctx, &rule.selectors) {
            matched.push((specificity, rule_order, &rule.declarations));
        }
    }

    // Lower-priority rules are applied first so later, more specific matches overwrite them.
    matched.sort_by_key(|(specificity, rule_order, _)| (*specificity, *rule_order));

    let mut values = default_values(document, node_id);
    // Presentational hints fold in between UA defaults and any author rules —
    // the HTML spec calls them "presentational hints" and gives them less
    // weight than every selector match, so a `<table border="1">` value
    // surrenders to `table { border: none; }` in author CSS but still wins
    // over an unstyled UA fallback.
    if let Some(NodeType::Element(elem_data)) = document.get(node_id).map(|n| &n.node_type) {
        for (name, value) in presentational_hints(elem_data) {
            values.insert(name, value);
        }
    }
    for (_, _, declarations) in matched {
        apply_declarations(&mut values, declarations);
    }

    // Legacy `<center>` element: real browsers center every block child
    // through quirks-mode magic (effectively `margin: 0 auto`). HN still
    // wraps its main table in `<center>` for that reason. Without this
    // shim the table renders left-aligned, leaving a wide empty band on
    // the right. We only fill in auto-margins when the cascade hasn't
    // already supplied a horizontal margin — author CSS / presentational
    // hints stay authoritative if they declared one.
    if parent_is_center(document, node_id)
        && !values.contains_key("margin-left")
        && !values.contains_key("margin-right")
    {
        values.insert("margin-left".into(), Value::Keyword("auto".into()));
        values.insert("margin-right".into(), Value::Keyword("auto".into()));
    }

    values
}

fn parent_is_center(document: &Document, node_id: NodeId) -> bool {
    let Some(parent_id) = document.get(node_id).and_then(|n| n.parent) else {
        return false;
    };
    matches!(
        document.get(parent_id).map(|n| &n.node_type),
        Some(NodeType::Element(element)) if element.tag_name.eq_ignore_ascii_case("center")
    )
}

// Re-export of the selector impl so the MatchingContext type parameter
// stays readable in `specified_values`. Keeps all selectors-crate
// generics on a single line per use-site.
type MiniBrowserSelectorImplAlias = crate::css::MiniBrowserSelectorImpl;

/// Translate the legacy presentational HTML attributes (`bgcolor`, `width`,
/// `align`, …) into equivalent CSS declarations. Pages from the table-layout
/// era still ship these instead of CSS, and modern frameworks emit them too
/// (newsletters, Wikipedia infoboxes). Mapping them here at style time lets
/// the rest of the engine — layout, render, getComputedStyle-style queries —
/// stay attribute-blind.
fn presentational_hints(element: &ElementData) -> PropertyMap {
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

fn default_values(document: &Document, node_id: NodeId) -> PropertyMap {
    let mut values = PropertyMap::new();
    let element = match document.get(node_id).map(|n| &n.node_type) {
        Some(NodeType::Element(element)) => element,
        _ => return values,
    };

    // These defaults act like a tiny user-agent stylesheet so unstyled pages remain legible.
    match element.tag_name.as_str() {
        // HTML5 default: these are non-rendered "metadata" / scripting elements.
        // Without this, a `<script>` body shows up as raw text in the page (the
        // single biggest visual noise on naver-style sites). The full set
        // matches the spec category for "metadata content + scripting".
        "head" | "title" | "meta" | "link" | "script" | "style" | "noscript" => {
            values.insert("display".into(), Value::Keyword("none".into()));
        }
        "body" => {
            edge_defaults(&mut values, "margin", 8.0);
        }
        // Legacy `<center>`: text-align centers inline descendants
        // (text, inline images, inline-block buttons). Centering of
        // block children — the historical reason this tag still
        // appears on real pages — is handled in `specified_values`
        // by injecting `margin-left/right: auto` on those children.
        "center" => {
            values.insert("text-align".into(), Value::Keyword("center".into()));
        }
        "p" => {
            values.insert(
                "margin-top".into(),
                Value::Length(12.0, crate::css::Unit::Px),
            );
            values.insert(
                "margin-bottom".into(),
                Value::Length(12.0, crate::css::Unit::Px),
            );
        }
        "h1" => {
            values.insert(
                "font-size".into(),
                Value::Length(32.0, crate::css::Unit::Px),
            );
            values.insert(
                "margin-top".into(),
                Value::Length(12.0, crate::css::Unit::Px),
            );
            values.insert(
                "margin-bottom".into(),
                Value::Length(16.0, crate::css::Unit::Px),
            );
        }
        "a" => {
            values.insert(
                "color".into(),
                Value::Color(crate::css::Color {
                    r: 0,
                    g: 102,
                    b: 204,
                    a: 255,
                }),
            );
            // The visual underline already lives in display_list (a link with
            // an href emits an underline command unless `text-decoration: none`
            // is in scope). Surfacing it here as a UA default makes the cascade
            // spec-correct: author CSS or runtime style queries see the same
            // value the renderer is acting on.
            values.insert("text-decoration".into(), Value::Keyword("underline".into()));
        }
        // <input> and <textarea> both render as atomic inline-block
        // widgets. The UA stylesheet gives them a fixed default width
        // (so an unstyled field still has a usable click target), a
        // 1px gray border + white background so the box silhouette
        // reads as a text field, and small horizontal padding so the
        // caret + value text don't kiss the border. <textarea> uses
        // the same shell — its multi-line behaviour is purely about
        // how `intrinsic_height` and the value-text commands handle
        // the value buffer. Author CSS still wins because UA defaults
        // are applied before matched declarations.
        // Table-family tags get the display values that flip them onto the
        // dedicated table layout path. Without these the parser still produces
        // the right tree shape but every <td> would render as a block, so
        // tabular content collapses into a single column. Author CSS still
        // wins because UA defaults run before matched declarations — pages
        // that explicitly do `table { display: block }` (mobile reflow trick)
        // still get the override they expect.
        //
        // thead/tbody/tfoot map to `table-row-group`; the table layout walker
        // treats those groups as transparent and harvests their <tr> children
        // directly. caption / col / colgroup are not yet handled by the
        // layout walker, so they fall through to default block rendering.
        "table" => {
            values.insert("display".into(), Value::Keyword("table".into()));
            // Default border-spacing matches HTML's traditional 2px gap
            // between cells. presentational_hints already overrides this
            // when `cellspacing` is on the tag — and author CSS overrides
            // both.
            values.insert(
                "border-spacing".into(),
                Value::Length(2.0, crate::css::Unit::Px),
            );
            // CSS spec says text-align inherits, but real browsers reset
            // it at the table boundary so an outer `<center>` (or any
            // ancestor with `text-align: center`) does not centre every
            // cell's content. Without this reset, HN inside `<center>`
            // ends up with all cell text centred — visually wrong even
            // though the cell columns themselves align correctly.
            values.insert("text-align".into(), Value::Keyword("left".into()));
        }
        "thead" | "tbody" | "tfoot" => {
            values.insert(
                "display".into(),
                Value::Keyword("table-row-group".into()),
            );
        }
        "tr" => {
            values.insert("display".into(), Value::Keyword("table-row".into()));
        }
        "td" | "th" => {
            values.insert("display".into(), Value::Keyword("table-cell".into()));
            // No UA padding default for now: the toy CSS parser doesn't expand
            // the `padding` shorthand, so a non-zero default here would be
            // permanently locked in for any page that resets cell padding via
            // `td { padding: 0 }`. Real browsers default to ~1px; once
            // shorthand expansion lands we can restore that.
        }
        // Real browsers give `<pre>` `white-space: pre`, a monospace font,
        // and a small vertical margin. Without these UA defaults a code
        // block on an unstyled page collapses its newlines (looks like one
        // long line) and renders in the proportional fallback (visually
        // indistinguishable from prose). Padding + a faint background let
        // the block read as a code panel even before any author CSS lands;
        // author rules win because UA defaults are applied before matched
        // declarations.
        "pre" => {
            values.insert("white-space".into(), Value::Keyword("pre".into()));
            values.insert("font-family".into(), Value::Keyword("monospace".into()));
            values.insert(
                "margin-top".into(),
                Value::Length(12.0, crate::css::Unit::Px),
            );
            values.insert(
                "margin-bottom".into(),
                Value::Length(12.0, crate::css::Unit::Px),
            );
            values.insert(
                "padding-top".into(),
                Value::Length(8.0, crate::css::Unit::Px),
            );
            values.insert(
                "padding-bottom".into(),
                Value::Length(8.0, crate::css::Unit::Px),
            );
            values.insert(
                "padding-left".into(),
                Value::Length(10.0, crate::css::Unit::Px),
            );
            values.insert(
                "padding-right".into(),
                Value::Length(10.0, crate::css::Unit::Px),
            );
            values.insert(
                "background-color".into(),
                Value::Color(crate::css::Color {
                    r: 246,
                    g: 248,
                    b: 250,
                    a: 255,
                }),
            );
        }
        // The HTML phrasing tags whose UA stylesheet is `font-family:
        // monospace`. Inline whitelist (layout::inline::is_inline_node)
        // already keeps them on the same line as surrounding text — what
        // they were missing was the family signal the renderer needs to
        // pick the monospace fallback. No bg / padding here: real browsers
        // leave that to author CSS, and bundling it would visually
        // double-up inside `<pre><code>` blocks (which already paint the
        // `<pre>` chrome).
        "code" | "kbd" | "samp" | "tt" => {
            values.insert("font-family".into(), Value::Keyword("monospace".into()));
        }
        "input" | "textarea" => {
            values.insert(
                "display".into(),
                Value::Keyword("inline-block".into()),
            );
            values.insert(
                "width".into(),
                Value::Length(200.0, crate::css::Unit::Px),
            );
            values.insert(
                "background-color".into(),
                Value::Color(crate::css::Color {
                    r: 255,
                    g: 255,
                    b: 255,
                    a: 255,
                }),
            );
            values.insert(
                "color".into(),
                Value::Color(crate::css::Color {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 255,
                }),
            );
            edge_defaults(&mut values, "border", 1.0);
            values.insert(
                "border-color".into(),
                Value::Color(crate::css::Color {
                    r: 118,
                    g: 118,
                    b: 118,
                    a: 255,
                }),
            );
            values.insert(
                "padding-left".into(),
                Value::Length(4.0, crate::css::Unit::Px),
            );
            values.insert(
                "padding-right".into(),
                Value::Length(4.0, crate::css::Unit::Px),
            );
            values.insert(
                "padding-top".into(),
                Value::Length(2.0, crate::css::Unit::Px),
            );
            values.insert(
                "padding-bottom".into(),
                Value::Length(2.0, crate::css::Unit::Px),
            );
        }
        _ => {}
    }

    values
}

fn edge_defaults(values: &mut PropertyMap, prefix: &str, amount: f32) {
    for side in ["top", "right", "bottom", "left"] {
        values.insert(
            format!("{prefix}-{side}"),
            Value::Length(amount, crate::css::Unit::Px),
        );
    }
}

fn apply_declarations(values: &mut PropertyMap, declarations: &[Declaration]) {
    // Later declarations with the same property name overwrite earlier ones.
    for declaration in declarations {
        values.insert(declaration.name.clone(), declaration.value.clone());
    }
}

/// Substitute every `Value::Var` in `values` with the looked-up `--*` value
/// in the same map. Custom properties inherit, so by the time this runs the
/// parent's declarations have already been folded in by the cascade caller.
///
/// Resolution is iterative: a variable that resolves to another variable is
/// chased again, with a `seen` set guarding against cycles. The fallback
/// branch fires only when the named property isn't present at all; once
/// substitution lands on something that *is* present, we use it even if the
/// caller also supplied a fallback. Composite values (gradients, shadows,
/// transforms, …) are not walked into — only top-level Var values are
/// substituted, which covers `color: var(--accent)` style use which is what
/// 5.1's site-color recovery target needs.
fn resolve_var_references(values: &mut PropertyMap) {
    let custom_props: HashMap<String, Value> = values
        .iter()
        .filter(|(name, _)| name.starts_with("--"))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();

    for (name, value) in values.iter_mut() {
        if name.starts_with("--") {
            // Custom-property *definitions* are kept as-is so descendants
            // that inherit them still see the original (possibly Var)
            // value. Each descendant runs its own resolve pass.
            continue;
        }
        resolve_var_value(value, &custom_props);
    }
}

fn resolve_var_value(value: &mut Value, custom_props: &HashMap<String, Value>) {
    let mut seen: HashSet<String> = HashSet::new();
    while let Value::Var { name, fallback } = value {
        if !seen.insert(name.clone()) {
            // Cycle detected — collapse to the spec-defined "initial" sentinel.
            *value = Value::Keyword("initial".into());
            return;
        }
        match custom_props.get(name) {
            Some(resolved) => {
                *value = resolved.clone();
            }
            None => {
                let fb = fallback.take();
                *value = match fb {
                    Some(boxed) => *boxed,
                    None => Value::Keyword("initial".into()),
                };
            }
        }
    }
}

fn matching_specificity(
    element: &MatchingElement<'_>,
    ctx: &mut MatchingContext<'_, MiniBrowserSelectorImplAlias>,
    selectors: &Selector,
) -> Option<u32> {
    // The selectors crate hands us the parsed `SelectorList` (one entry
    // per `Rule`'s comma-separated selector list). `matches_selector_list`
    // returns true if any branch matches; for the cascade we also want
    // the *highest* specificity among the matching branches, so we walk
    // the list ourselves with `matches_selector` instead.
    let mut best: Option<u32> = None;
    for selector in selectors.list().slice() {
        if selectors::matching::matches_selector(selector, 0, None, element, ctx) {
            let spec = selector.specificity();
            best = Some(best.map_or(spec, |prev| prev.max(spec)));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use crate::{
        css::{Color, Unit, Value},
        dom::{Document, NodeId},
        html, style,
    };

    fn parse_html(source: &str) -> (Document, NodeId) {
        let document = html::parse(source).unwrap();
        let root = document.roots()[0];
        (document, root)
    }

    fn parse_css(source: &str) -> crate::css::Stylesheet {
        crate::css::parse(source).unwrap()
    }

    #[test]
    fn applies_rule_specificity_in_tag_class_id_order() {
        let (document, root) = parse_html(r#"<div id="hero" class="card promo">Hello</div>"#);
        let stylesheet = parse_css(
            r#"
                div { color: #111111; display: block; }
                .promo { color: #222222; }
                #hero { color: #333333; }
            "#,
        );

        let styled = style::style_tree(&document, root, &[stylesheet]);

        assert_eq!(
            styled.value("color"),
            Some(&Value::Color(Color {
                r: 51,
                g: 51,
                b: 51,
                a: 255,
            }))
        );
        assert_eq!(
            styled.value("display"),
            Some(&Value::Keyword("block".into()))
        );
    }

    #[test]
    fn inherits_color_and_font_size_from_parent() {
        let (document, root) = parse_html(r#"<div id="app"><span>Text</span></div>"#);
        let stylesheet = parse_css(
            r#"
                #app {
                    color: #ff0000;
                    font-size: 18px;
                }
            "#,
        );

        let styled = style::style_tree(&document, root, &[stylesheet]);
        let child = &styled.children[0];

        assert_eq!(
            child.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
        assert_eq!(
            child.value("font-size"),
            Some(&Value::Length(18.0, Unit::Px))
        );
    }

    #[test]
    fn text_nodes_inherit_parent_style() {
        let (document, root) = parse_html(r#"<p class="copy">Hello</p>"#);
        let stylesheet = parse_css(
            r#"
                .copy {
                    color: #0f0;
                }
            "#,
        );

        let styled = style::style_tree(&document, root, &[stylesheet]);
        let text = &styled.children[0];

        assert_eq!(
            text.value("color"),
            Some(&Value::Color(Color {
                r: 0,
                g: 255,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn applies_basic_user_agent_defaults() {
        let (document, root) =
            parse_html(r#"<body><h1>Title</h1><p>Copy</p><a href="/next">Next</a></body>"#);
        let styled = style::style_tree(&document, root, &[]);

        assert_eq!(
            styled.value("margin-top"),
            Some(&Value::Length(8.0, Unit::Px))
        );
        assert_eq!(
            styled.children[0].value("font-size"),
            Some(&Value::Length(32.0, Unit::Px))
        );
        assert_eq!(
            styled.children[1].value("margin-bottom"),
            Some(&Value::Length(12.0, Unit::Px))
        );
        assert_eq!(
            styled.children[2].value("color"),
            Some(&Value::Color(Color {
                r: 0,
                g: 102,
                b: 204,
                a: 255,
            }))
        );
        // <a> also defaults to text-decoration: underline so the cascade matches
        // the underline the renderer paints; queries on specified style see it.
        assert_eq!(
            styled.children[2].value("text-decoration"),
            Some(&Value::Keyword("underline".into()))
        );
    }

    #[test]
    fn anchor_text_decoration_default_does_not_inherit_to_children() {
        // text-decoration is not on the inherit list, so a <span> inside an <a>
        // does NOT pick up the underline default. (display_list still emits
        // underline commands for the link's text descendants — that's a render
        // concern, not a style cascade one.)
        let (document, root) = parse_html(r#"<a href="/x"><span>label</span></a>"#);
        let styled = style::style_tree(&document, root, &[]);

        assert_eq!(
            styled.value("text-decoration"),
            Some(&Value::Keyword("underline".into()))
        );
        assert_eq!(styled.children[0].value("text-decoration"), None);
    }

    #[test]
    fn input_gets_widget_user_agent_defaults() {
        // <input> needs a visible silhouette without any author CSS, so the
        // UA default sketches it as a 200px-wide inline-block with white bg,
        // gray border, and small horizontal padding. This is what makes an
        // unstyled <input> render as a recognizable text field rather than
        // collapsing to a zero-sized text node.
        let (document, root) =
            parse_html(r#"<div><input type="text" value="hello"/></div>"#);
        let styled = style::style_tree(&document, root, &[]);
        let input = &styled.children[0];

        assert_eq!(
            input.value("display"),
            Some(&Value::Keyword("inline-block".into()))
        );
        assert_eq!(
            input.value("width"),
            Some(&Value::Length(200.0, Unit::Px))
        );
        assert_eq!(
            input.value("background-color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 255,
                b: 255,
                a: 255,
            }))
        );
        assert_eq!(
            input.value("border-color"),
            Some(&Value::Color(Color {
                r: 118,
                g: 118,
                b: 118,
                a: 255,
            }))
        );
        // 1px border on every side so border_commands actually paints.
        assert_eq!(
            input.value("border-top"),
            Some(&Value::Length(1.0, Unit::Px))
        );
        assert_eq!(
            input.value("padding-left"),
            Some(&Value::Length(4.0, Unit::Px))
        );
    }

    #[test]
    fn input_user_agent_defaults_are_overridable_by_author_styles() {
        // UA defaults run before matched declarations, so an author rule that
        // declares `border-color` or `width` on an input still wins. Same
        // override mechanism `body { margin-top: ...}` already relies on.
        let (document, root) = parse_html(r#"<input type="text"/>"#);
        let stylesheet = parse_css(
            r#"
                input {
                    width: 320px;
                    border-color: #ff0000;
                }
            "#,
        );
        let styled = style::style_tree(&document, root, &[stylesheet]);
        assert_eq!(
            styled.value("width"),
            Some(&Value::Length(320.0, Unit::Px))
        );
        assert_eq!(
            styled.value("border-color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn metadata_and_script_tags_default_to_display_none() {
        // <head>, <title>, <meta>, <link>, <script>, <style>, <noscript> are
        // non-rendered per HTML5; without this UA default a `<script>` body
        // shows up as raw text in the page, which dominates the visual noise
        // on real-world pages like the naver landing page.
        let (document, root) = parse_html(
            r#"<body><script>var x = 1;</script><style>p{color:red}</style><div>visible</div></body>"#,
        );
        let styled = style::style_tree(&document, root, &[]);

        let none = Value::Keyword("none".into());
        let script = &styled.children[0];
        let style_tag = &styled.children[1];
        let div = &styled.children[2];

        assert_eq!(script.value("display"), Some(&none));
        assert_eq!(style_tag.value("display"), Some(&none));
        // The visible sibling is unaffected.
        assert_ne!(div.value("display"), Some(&none));
    }

    #[test]
    fn descendant_selector_matches_nested_target() {
        let (document, root) = parse_html(
            r#"<div class="outer"><section><span class="inner">hi</span></section></div>"#,
        );
        let stylesheet = parse_css(".outer .inner { color: #ff0000; }");
        let styled = style::style_tree(&document, root, &[stylesheet]);
        let inner = &styled.children[0].children[0];

        // .outer .inner targets the <span>, even though a <section> sits between them.
        assert_eq!(
            inner.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn descendant_selector_does_not_match_when_ancestor_is_missing() {
        let (document, root) = parse_html(r#"<div><span class="inner">hi</span></div>"#);
        let stylesheet = parse_css(".outer .inner { color: #ff0000; }");
        let styled = style::style_tree(&document, root, &[stylesheet]);
        let inner = &styled.children[0];

        // No `.outer` ancestor exists, so the rule must not apply.
        assert_eq!(inner.value("color"), None);
    }

    #[test]
    fn child_selector_matches_only_immediate_parent() {
        // .outer > .inner should NOT match when a <section> sits between the two —
        // unlike descendant, the child combinator forbids skipping.
        let (document, root) = parse_html(
            r#"<div class="outer"><section><span class="inner">hi</span></section></div>"#,
        );
        let stylesheet = parse_css(".outer > .inner { color: #ff0000; }");
        let styled = style::style_tree(&document, root, &[stylesheet]);
        let inner = &styled.children[0].children[0];

        assert_eq!(inner.value("color"), None);
    }

    #[test]
    fn child_selector_matches_when_parent_is_direct() {
        let (document, root) =
            parse_html(r#"<div class="outer"><span class="inner">hi</span></div>"#);
        let stylesheet = parse_css(".outer > .inner { color: #ff0000; }");
        let styled = style::style_tree(&document, root, &[stylesheet]);
        let inner = &styled.children[0];

        assert_eq!(
            inner.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn mixed_descendant_and_child_combinators_compose_correctly() {
        // `nav ul > li` requires: target is <li>, its parent is <ul>, and somewhere up
        // the chain a <nav> ancestor exists.
        let (document, root) =
            parse_html(r#"<nav class="primary"><div><ul><li class="t">hi</li></ul></div></nav>"#);
        let stylesheet = parse_css("nav ul > li { color: #ff0000; }");
        let styled = style::style_tree(&document, root, &[stylesheet]);
        let li = &styled.children[0].children[0].children[0];

        assert_eq!(
            li.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn hover_pseudo_class_matches_when_hovered_path_targets_node() {
        // Build: <div><a class="btn">click</a></div>. The root has one child (the <a>),
        // so the <a>'s DOM path is [0]. Telling style_tree_with_hover that [0] is hovered
        // should activate the .btn:hover rule.
        let (document, root) = parse_html(r#"<div><a class="btn">click</a></div>"#);
        let stylesheet = parse_css(
            r#"
                .btn { color: #00ff00; }
                .btn:hover { color: #ff0000; }
            "#,
        );
        let styled = style::style_tree_with_hover(&document, root, &[stylesheet], Some(&[0]));
        let link = &styled.children[0];

        assert_eq!(
            link.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn hover_pseudo_class_only_applies_to_the_hovered_node_not_siblings() {
        // Two .btn siblings; only the first ([0,0]) is "hovered". The second should keep
        // the non-hover color, proving the hovered_path identifies a single node.
        let (document, root) = parse_html(r#"<div><a class="btn">a</a><a class="btn">b</a></div>"#);
        let stylesheet = parse_css(
            r#"
                .btn { color: #00ff00; }
                .btn:hover { color: #ff0000; }
            "#,
        );
        let styled = style::style_tree_with_hover(&document, root, &[stylesheet], Some(&[0]));
        let first = &styled.children[0];
        let second = &styled.children[1];

        assert_eq!(
            first.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
        assert_eq!(
            second.value("color"),
            Some(&Value::Color(Color {
                r: 0,
                g: 255,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn hover_on_ancestor_propagates_through_descendant_combinator() {
        // .outer:hover .inner — when the .outer ancestor is hovered, the descendant
        // .inner picks up the rule even though .inner itself isn't under the cursor.
        let (document, root) =
            parse_html(r#"<div class="outer"><span class="inner">x</span></div>"#);
        let stylesheet = parse_css(
            r#"
                .inner { color: #00ff00; }
                .outer:hover .inner { color: #ff0000; }
            "#,
        );
        // Path [] is the root <div class="outer">.
        let styled = style::style_tree_with_hover(&document, root, &[stylesheet], Some(&[]));
        let inner = &styled.children[0];

        assert_eq!(
            inner.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn focus_pseudo_class_matches_only_the_focused_node_not_ancestors() {
        // <div><a class="btn">click</a></div>; mark the .btn at path [0] as focused.
        // The .btn rule must fire, but focus does NOT bubble: a hypothetical .root:focus
        // wouldn't match the outer <div>. We assert the positive case here; the negative
        // is covered by the "no engaged path" test.
        let (document, root) = parse_html(r#"<div><a class="btn">click</a></div>"#);
        let stylesheet = parse_css(
            r#"
                .btn { color: #00ff00; }
                .btn:focus { color: #ff0000; }
            "#,
        );
        let styled = style::style_tree_with_state(
            &document,
            root,
            &[stylesheet],
            style::InteractionState {
                focus: Some(&[0]),
                ..Default::default()
            },
        );
        let link = &styled.children[0];

        assert_eq!(
            link.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn focus_pseudo_class_does_not_propagate_to_ancestors() {
        // Hover the deepest text under .outer; .outer:focus should NOT match because
        // focus is anchored to the focused node alone, not its ancestor chain. We use
        // the focus path of the deeper text node and verify .outer keeps its non-focus color.
        let (document, root) =
            parse_html(r#"<div class="outer"><span class="inner">x</span></div>"#);
        let stylesheet = parse_css(
            r#"
                .outer { color: #00ff00; }
                .outer:focus { color: #ff0000; }
            "#,
        );
        // Focus path [0, 0] = the text node inside .inner. .outer is the root.
        let styled = style::style_tree_with_state(
            &document,
            root,
            &[stylesheet],
            style::InteractionState {
                focus: Some(&[0, 0]),
                ..Default::default()
            },
        );

        // .outer keeps the green non-focus color even though the focused node is its
        // grand-descendant. The :hover equivalent would have matched here.
        assert_eq!(
            styled.value("color"),
            Some(&Value::Color(Color {
                r: 0,
                g: 255,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn active_pseudo_class_propagates_like_hover() {
        // .active matches both the deepest active node and its ancestors, mirroring
        // :hover semantics.
        let (document, root) =
            parse_html(r#"<div class="outer"><span class="inner">x</span></div>"#);
        let stylesheet = parse_css(
            r#"
                .outer { color: #00ff00; }
                .outer:active { color: #ff0000; }
            "#,
        );
        // Active path [0, 0] = the text node; .outer (path []) is its ancestor.
        let styled = style::style_tree_with_state(
            &document,
            root,
            &[stylesheet],
            style::InteractionState {
                active: Some(&[0, 0]),
                ..Default::default()
            },
        );

        assert_eq!(
            styled.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn hover_propagates_from_deeply_hovered_descendant_to_ancestors() {
        // Deepest hovered node is the text inside .btn (path [0, 0]). The CSS spec says
        // every ancestor on the way down also enters :hover, so the .btn rule should
        // apply even though the cursor is over its text child.
        let (document, root) = parse_html(r#"<a class="btn">click</a>"#);
        let stylesheet = parse_css(
            r#"
                .btn { color: #00ff00; }
                .btn:hover { color: #ff0000; }
            "#,
        );
        let styled = style::style_tree_with_hover(&document, root, &[stylesheet], Some(&[0]));

        assert_eq!(
            styled.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn hover_pseudo_class_does_not_match_when_no_hover_path_is_given() {
        // The legacy entry point — `style_tree` without hover info — defaults to "nothing
        // is hovered", so any :hover rule should silently fail to match. A bare-class
        // fallback confirms the surrounding cascade still works.
        let (document, root) = parse_html(r#"<a class="btn">click</a>"#);
        let stylesheet = parse_css(
            r#"
                .btn { color: #00ff00; }
                .btn:hover { color: #ff0000; }
            "#,
        );
        let styled = style::style_tree(&document, root, &[stylesheet]);

        // The non-hover rule wins because the hover rule never matches.
        assert_eq!(
            styled.value("color"),
            Some(&Value::Color(Color {
                r: 0,
                g: 255,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn descendant_selector_specificity_sums_across_chain() {
        // .outer .inner has specificity 10 + 10 = 20, beating the lone .inner rule (10)
        // even when the latter is listed later in the stylesheet.
        let (document, root) =
            parse_html(r#"<div class="outer"><span class="inner">hi</span></div>"#);
        let stylesheet = parse_css(
            r#"
                .outer .inner { color: #ff0000; }
                .inner { color: #00ff00; }
            "#,
        );
        let styled = style::style_tree(&document, root, &[stylesheet]);
        let inner = &styled.children[0];

        assert_eq!(
            inner.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn em_resolves_against_parent_font_size_for_font_size_itself() {
        let (document, root) = parse_html(r#"<div id="outer"><div id="inner"></div></div>"#);
        let stylesheet = parse_css(
            r#"
                #outer { font-size: 20px; }
                #inner { font-size: 1.5em; }
            "#,
        );
        let styled = style::style_tree(&document, root, &[stylesheet]);
        let inner = &styled.children[0];

        // 1.5em on a 20px parent resolves to 30px and is stored as a Px length so children
        // see it during their own cascade.
        assert_eq!(
            inner.value("font-size"),
            Some(&Value::Length(30.0, Unit::Px))
        );
    }

    #[test]
    fn em_on_other_properties_uses_own_resolved_font_size() {
        let (document, root) = parse_html(r#"<div id="outer"><div id="inner"></div></div>"#);
        let stylesheet = parse_css(
            r#"
                #outer { font-size: 20px; }
                #inner { font-size: 1.5em; padding-left: 2em; }
            "#,
        );
        let styled = style::style_tree(&document, root, &[stylesheet]);
        let inner = &styled.children[0];

        // padding 2em uses inner's resolved font-size (30px), not the parent's: 60px.
        assert_eq!(
            inner.value("padding-left"),
            Some(&Value::Length(60.0, Unit::Px))
        );
    }

    #[test]
    fn rem_resolves_against_root_font_size_regardless_of_depth() {
        let (document, root) = parse_html(
            r#"<div id="root"><div class="middle"><div class="leaf"></div></div></div>"#,
        );
        let stylesheet = parse_css(
            r#"
                #root { font-size: 24px; }
                .middle { font-size: 12px; }
                .leaf { padding-left: 0.5rem; }
            "#,
        );
        let styled = style::style_tree(&document, root, &[stylesheet]);
        let leaf = &styled.children[0].children[0];

        // 0.5rem references the root font-size (24px), independent of the .middle ancestor.
        assert_eq!(
            leaf.value("padding-left"),
            Some(&Value::Length(12.0, Unit::Px))
        );
    }

    #[test]
    fn percent_on_non_font_properties_stays_unresolved_until_layout() {
        let (document, root) = parse_html(r#"<div class="card"></div>"#);
        let stylesheet = parse_css(".card { width: 50%; }");
        let styled = style::style_tree(&document, root, &[stylesheet]);

        // Percent on width is held back for the layout layer to resolve against the
        // containing block's content width.
        assert_eq!(
            styled.value("width"),
            Some(&Value::Length(50.0, Unit::Percent))
        );
    }

    #[test]
    fn pre_gets_monospace_white_space_padding_and_background_ua_defaults() {
        // Phase 5.5: a bare `<pre>` should arrive at the renderer with the
        // four UA defaults that turn it into a recognizable code block —
        // monospace family, `white-space: pre` so newlines survive,
        // padding so the text doesn't kiss the edge, and a faint
        // background so the block reads as a panel even with no author
        // CSS. Without these, a code-heavy page (Haskell Blog) renders
        // its code blocks as one-line proportional prose.
        let (document, root) = parse_html(r#"<pre>x</pre>"#);
        let styled = style::style_tree(&document, root, &[]);

        assert_eq!(
            styled.value("white-space"),
            Some(&Value::Keyword("pre".into()))
        );
        assert_eq!(
            styled.value("font-family"),
            Some(&Value::Keyword("monospace".into()))
        );
        assert_eq!(
            styled.value("padding-left"),
            Some(&Value::Length(10.0, Unit::Px))
        );
        assert_eq!(
            styled.value("padding-top"),
            Some(&Value::Length(8.0, Unit::Px))
        );
        assert_eq!(
            styled.value("background-color"),
            Some(&Value::Color(Color {
                r: 246,
                g: 248,
                b: 250,
                a: 255,
            }))
        );
    }

    #[test]
    fn code_kbd_samp_tt_get_monospace_font_family_ua_default() {
        // Phase 5.5: the four phrasing tags whose UA stylesheet maps to
        // `font-family: monospace`. The renderer routes shaping through
        // cosmic-text's Family::Monospace whenever the cascaded keyword is
        // `monospace`, so without this default an unstyled `<code>` would
        // shape with the proportional fallback even though the tag's
        // semantic meaning is "verbatim source text".
        for tag in ["code", "kbd", "samp", "tt"] {
            let (document, root) = parse_html(&format!("<{tag}>x</{tag}>"));
            let styled = style::style_tree(&document, root, &[]);
            assert_eq!(
                styled.value("font-family"),
                Some(&Value::Keyword("monospace".into())),
                "{tag} should default to font-family: monospace",
            );
        }
    }

    #[test]
    fn pre_font_family_inherits_to_text_and_nested_code_child() {
        // The renderer reads `font-family` off each text-bearing leaf, so
        // the UA default on `<pre>` only matters if it inherits down.
        // This locks in the contract for both the immediate text child
        // and a `<pre><code>...` nesting (the canonical code-block
        // markup) — both must see `monospace` on the leaf the painter
        // actually shapes.
        let (document, root) = parse_html(r#"<pre><code>x</code></pre>"#);
        let styled = style::style_tree(&document, root, &[]);
        let code = &styled.children[0];
        let text = &code.children[0];

        assert_eq!(
            code.value("font-family"),
            Some(&Value::Keyword("monospace".into()))
        );
        assert_eq!(
            text.value("font-family"),
            Some(&Value::Keyword("monospace".into()))
        );
    }

    #[test]
    fn white_space_inherits_from_parent_to_text_child() {
        // The text-collapse helper reads `white-space` off the *text*
        // node's specified style. Without inheritance, a parent <pre>
        // declaring `white-space: pre` wouldn't reach the text child
        // and the renderer would still collapse newlines.
        let (document, root) = parse_html(r#"<pre>line one</pre>"#);
        let stylesheet = parse_css("pre { white-space: pre; }");
        let styled = style::style_tree(&document, root, &[stylesheet]);
        let text_child = &styled.children[0];

        assert_eq!(
            text_child.value("white-space"),
            Some(&Value::Keyword("pre".into()))
        );
    }

    #[test]
    fn presentational_bgcolor_attribute_maps_to_background_color() {
        // Legacy `<body bgcolor="...">` style: bgcolor takes named, hex, and
        // bare-hex (no `#`) values. The cascade picks each up so the painted
        // background matches what HTML 4 / Wikipedia infoboxes intended.
        let (document, body) = parse_html(r##"<body bgcolor="#ffeeaa"></body>"##);
        let styled = style::style_tree(&document, body, &[]);
        assert_eq!(
            styled.value("background-color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 238,
                b: 170,
                a: 255,
            }))
        );

        let (document, body) = parse_html(r#"<body bgcolor="white"></body>"#);
        let styled = style::style_tree(&document, body, &[]);
        assert_eq!(
            styled.value("background-color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 255,
                b: 255,
                a: 255,
            }))
        );

        let (document, body) = parse_html(r#"<body bgcolor="ff0000"></body>"#);
        let styled = style::style_tree(&document, body, &[]);
        assert_eq!(
            styled.value("background-color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn presentational_width_height_map_to_length_or_percent() {
        // `<img width="200" height="100">` and `<table width="50%">` are the
        // two shapes that real pages still use today. Bare digits become
        // px lengths; trailing `%` produces a percent length so layout
        // resolves it against the containing block.
        let (document, img) = parse_html(r#"<img src="x.png" width="200" height="100"/>"#);
        let styled = style::style_tree(&document, img, &[]);
        assert_eq!(styled.value("width"), Some(&Value::Length(200.0, Unit::Px)));
        assert_eq!(styled.value("height"), Some(&Value::Length(100.0, Unit::Px)));

        let (document, table) = parse_html(r#"<table width="50%"></table>"#);
        let styled = style::style_tree(&document, table, &[]);
        assert_eq!(
            styled.value("width"),
            Some(&Value::Length(50.0, Unit::Percent))
        );
    }

    #[test]
    fn presentational_align_maps_to_text_align_or_float_per_tag() {
        // On block-level / table-section tags `align="center"` becomes
        // text-align; on floatable embeds (img/table) only `left`/`right`
        // map to CSS float, since there's no clean "float center" in CSS.
        let (document, p) = parse_html(r#"<p align="center">hi</p>"#);
        let styled = style::style_tree(&document, p, &[]);
        assert_eq!(
            styled.value("text-align"),
            Some(&Value::Keyword("center".into()))
        );

        let (document, img) = parse_html(r#"<img src="x.png" align="right"/>"#);
        let styled = style::style_tree(&document, img, &[]);
        assert_eq!(
            styled.value("float"),
            Some(&Value::Keyword("right".into()))
        );

        // align="center" on an <img> isn't translated — modern equivalent
        // is `margin: auto`, which our layout doesn't honor for floats.
        let (document, img) = parse_html(r#"<img src="x.png" align="center"/>"#);
        let styled = style::style_tree(&document, img, &[]);
        assert_eq!(styled.value("float"), None);
        assert_eq!(styled.value("text-align"), None);
    }

    #[test]
    fn presentational_border_attribute_emits_uniform_border_with_solid_style() {
        // `<table border="1">` is the canonical "give me a visible grid"
        // shorthand. Real browsers expand it to a 1px solid border on
        // every side; we match that so an unstyled markup table at least
        // shows its outer border (cell borders need full table layout).
        let (document, table) = parse_html(r#"<table border="2"></table>"#);
        let styled = style::style_tree(&document, table, &[]);
        assert_eq!(
            styled.value("border-top"),
            Some(&Value::Length(2.0, Unit::Px))
        );
        assert_eq!(
            styled.value("border-right"),
            Some(&Value::Length(2.0, Unit::Px))
        );
        assert_eq!(
            styled.value("border-bottom"),
            Some(&Value::Length(2.0, Unit::Px))
        );
        assert_eq!(
            styled.value("border-left"),
            Some(&Value::Length(2.0, Unit::Px))
        );
        assert_eq!(
            styled.value("border-style"),
            Some(&Value::Keyword("solid".into()))
        );

        // border="0" is the "drop the link border on an image" idiom; emit
        // explicit zero lengths but skip the solid keyword (a 0-width
        // border has no visible style).
        let (document, img) = parse_html(r#"<img src="x.png" border="0"/>"#);
        let styled = style::style_tree(&document, img, &[]);
        assert_eq!(
            styled.value("border-top"),
            Some(&Value::Length(0.0, Unit::Px))
        );
        assert_eq!(styled.value("border-style"), None);
    }

    #[test]
    fn presentational_hints_lose_to_author_css_but_beat_ua_defaults() {
        // The HTML spec gives presentational hints lower weight than every
        // selector match, so an explicit `body { background: ... }` rule
        // wins over `<body bgcolor="...">`. They still beat plain UA
        // defaults because there isn't one for background-color on body.
        let (document, body) = parse_html(r##"<body bgcolor="#ff0000"></body>"##);
        let stylesheet = parse_css("body { background-color: #00ff00; }");
        let styled = style::style_tree(&document, body, &[stylesheet]);
        assert_eq!(
            styled.value("background-color"),
            Some(&Value::Color(Color {
                r: 0,
                g: 255,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn author_styles_override_user_agent_defaults() {
        let (document, root) = parse_html(r#"<body><a href="/next">Next</a></body>"#);
        let stylesheet = parse_css(
            r#"
                body { margin-top: 20px; }
                a { color: #ff0000; }
            "#,
        );
        let styled = style::style_tree(&document, root, &[stylesheet]);

        assert_eq!(
            styled.value("margin-top"),
            Some(&Value::Length(20.0, Unit::Px))
        );
        assert_eq!(
            styled.children[0].value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn link_pseudo_matches_anchors_with_href_so_link_color_wins_over_visited() {
        // Real HN ships `a:link { color: black } a:visited { color: gray }`.
        // Without `:link`/`:visited` matching, both rules collapse to bare
        // `a` and source order makes the visited rule win — every link
        // (including unvisited story titles) renders gray. The fix:
        // `:link` matches every `<a href>` (we have no visited set), and
        // `:visited` never matches, so the `a:link` rule's value is the
        // one that survives the cascade.
        let (document, root) = parse_html(r#"<a href="/next">Next</a>"#);
        let stylesheet = parse_css(
            r#"
                a:link { color: #000000; }
                a:visited { color: #828282; }
            "#,
        );
        let styled = style::style_tree(&document, root, &[stylesheet]);

        assert_eq!(
            styled.value("color"),
            Some(&Value::Color(Color {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn link_pseudo_does_not_match_anchors_missing_href() {
        // CSS spec: `:link` only matches anchors that are hyperlinks
        // (i.e. carry an href). A bare `<a>name="…">` style fragment
        // should fall through, so the author's red `a:link` rule does
        // not apply — the anchor keeps the UA default `<a>` colour
        // (~#0066CC) instead of becoming red.
        let (document, root) = parse_html(r#"<a>Just a label</a>"#);
        let stylesheet = parse_css(r#"a:link { color: #ff0000; }"#);
        let styled = style::style_tree(&document, root, &[stylesheet]);

        assert_eq!(
            styled.value("color"),
            Some(&Value::Color(Color {
                r: 0,
                g: 102,
                b: 204,
                a: 255,
            })),
            "anchor without href must keep the UA-default link colour, not the author's :link rule"
        );
    }

    #[test]
    fn center_element_injects_auto_margins_on_block_children() {
        // HN still wraps its main table in `<body><center><table>…</table></center>`
        // and relies on the legacy quirks-mode behaviour where `<center>` makes
        // every block child auto-center. We approximate that with an
        // `margin-left/right: auto` injection on direct children of a
        // `<center>` parent — the existing block layout already centers
        // explicit-width boxes when both margins are auto.
        let (document, root) =
            parse_html(r#"<center><table><tr><td>x</td></tr></table></center>"#);
        let stylesheet = parse_css(r#""#);
        let styled = style::style_tree(&document, root, &[stylesheet]);
        let table = &styled.children[0];

        assert_eq!(
            table.value("margin-left"),
            Some(&Value::Keyword("auto".into())),
            "table inside <center> must get margin-left: auto"
        );
        assert_eq!(
            table.value("margin-right"),
            Some(&Value::Keyword("auto".into())),
            "table inside <center> must get margin-right: auto"
        );
    }

    #[test]
    fn center_element_respects_author_supplied_margins() {
        // The auto-margin injection only fires when the cascade hasn't
        // already supplied a horizontal margin, so author CSS that pins
        // the table to one side wins as expected.
        let (document, root) =
            parse_html(r#"<center><table id="hnmain"><tr><td>x</td></tr></table></center>"#);
        let stylesheet = parse_css(r#"#hnmain { margin-left: 10px; margin-right: 20px; }"#);
        let styled = style::style_tree(&document, root, &[stylesheet]);
        let table = &styled.children[0];

        assert_eq!(
            table.value("margin-left"),
            Some(&Value::Length(10.0, Unit::Px))
        );
        assert_eq!(
            table.value("margin-right"),
            Some(&Value::Length(20.0, Unit::Px))
        );
    }

    #[test]
    fn table_blocks_text_align_inheritance_from_center_ancestor() {
        // HN wraps its main table in `<center>`, which sets
        // `text-align: center`. CSS technically inherits text-align,
        // but real browsers stop the inheritance at the table boundary
        // — otherwise every cell's content would be centred. The UA
        // default `table { text-align: left }` is what blocks the
        // inheritance: it puts an explicit value on the table so the
        // cells beneath it inherit "left" instead of the ancestor's
        // "center".
        let (document, root) = parse_html(
            r#"<center><table><tr><td>cell</td></tr></table></center>"#,
        );
        let stylesheet = parse_css(r#""#);
        let styled = style::style_tree(&document, root, &[stylesheet]);
        // Walk: <center> → <table> → <tr> → <td>.
        let table = &styled.children[0];
        let tr = &table.children[0];
        let td = &tr.children[0];

        assert_eq!(
            table.value("text-align"),
            Some(&Value::Keyword("left".into())),
            "table itself must reset text-align"
        );
        assert_eq!(
            td.value("text-align"),
            Some(&Value::Keyword("left".into())),
            "cell must inherit the table's reset, not the <center> ancestor"
        );
    }

    #[test]
    fn descendant_link_selector_wins_over_bare_anchor_link() {
        // The HN cascade also relies on `.subtext a:link { color: gray }`
        // overriding the bare `a:link { color: black }` for anchors that
        // sit inside `.subtext`. The descendant selector has higher
        // specificity (one class + one tag) than the bare tag selector,
        // so its colour wins regardless of source order.
        let (document, root) = parse_html(
            r#"<div class="subtext"><a href="/x">child</a></div>"#,
        );
        let stylesheet = parse_css(
            r#"
                a:link { color: #000000; }
                .subtext a:link { color: #828282; }
            "#,
        );
        let styled = style::style_tree(&document, root, &[stylesheet]);
        let anchor = &styled.children[0];

        assert_eq!(
            anchor.value("color"),
            Some(&Value::Color(Color {
                r: 130,
                g: 130,
                b: 130,
                a: 255,
            }))
        );
    }

    #[test]
    fn ch_unit_resolves_to_half_font_size_in_px_at_default_root() {
        // Default UA font-size is 16px and our `ch` approximation is
        // `0.5 * font-size`, so `65ch` should land at 65 * 16 * 0.5 = 520px.
        let (document, root) = parse_html(r#"<article class="copy">x</article>"#);
        let stylesheet = parse_css(r#".copy { max-width: 65ch; }"#);

        let styled = style::style_tree(&document, root, &[stylesheet]);
        assert_eq!(
            styled.value("max-width"),
            Some(&Value::Length(520.0, Unit::Px))
        );
    }

    #[test]
    fn ch_unit_scales_with_local_font_size_on_the_same_node() {
        // The element's own font-size is what `ch` resolves against, even
        // when it differs from the inherited / root size — same rule we
        // already enforce for em on non-font-size properties.
        let (document, root) = parse_html(r#"<article class="copy">x</article>"#);
        let stylesheet = parse_css(
            r#"
                .copy {
                    font-size: 20px;
                    max-width: 65ch;
                }
            "#,
        );

        let styled = style::style_tree(&document, root, &[stylesheet]);
        // 65 * 20 * 0.5 = 650.
        assert_eq!(
            styled.value("max-width"),
            Some(&Value::Length(650.0, Unit::Px))
        );
    }

    #[test]
    fn pt_unit_resolves_to_four_thirds_px_in_cascade() {
        // 1pt = 1/72in, CSS pins 1in = 96px → 4/3 px. A `padding: 12pt`
        // declaration should land at exactly 16px, the same value Chrome
        // reports in DevTools for the same input.
        let (document, root) = parse_html(r#"<div class="card">x</div>"#);
        let stylesheet = parse_css(r#".card { padding-left: 12pt; }"#);

        let styled = style::style_tree(&document, root, &[stylesheet]);
        assert_eq!(
            styled.value("padding-left"),
            Some(&Value::Length(16.0, Unit::Px))
        );
    }

    #[test]
    fn pt_font_size_scales_inherited_em_lengths() {
        // HN-style: body `font-size: 10pt` → 13.333px. A child whose own
        // declarations use em should resolve against that, not the 16px UA
        // default. This is the regression Phase 6.A is built to fix —
        // before pt parsed, `10pt` fell through to a Keyword and the body
        // stayed at 16px.
        let (document, root) =
            parse_html(r#"<body class="page"><span class="lead">x</span></body>"#);
        let stylesheet = parse_css(
            r#"
                .page { font-size: 10pt; }
                .lead { padding-left: 2em; }
            "#,
        );

        let styled = style::style_tree(&document, root, &[stylesheet]);
        // body: 10pt → 40/3 ≈ 13.333px.
        match styled.value("font-size") {
            Some(Value::Length(v, Unit::Px)) => {
                assert!((v - 40.0 / 3.0).abs() < 1e-4, "got {v}");
            }
            other => panic!("unexpected font-size: {other:?}"),
        }
        // child padding-left: 2em against 13.333px → 80/3 ≈ 26.666px.
        let child = &styled.children[0];
        match child.value("padding-left") {
            Some(Value::Length(v, Unit::Px)) => {
                assert!((v - 80.0 / 3.0).abs() < 1e-4, "got {v}");
            }
            other => panic!("unexpected padding-left: {other:?}"),
        }
    }

    #[test]
    fn var_reference_resolves_against_custom_property_on_same_node() {
        // Both `--accent` and the consuming `color: var(--accent)` live on
        // the same rule, so the resolve pass finds it in the local map.
        let (document, root) = parse_html(r#"<div class="card">x</div>"#);
        let stylesheet = parse_css(
            r#"
                .card {
                    --accent: #7e2882;
                    color: var(--accent);
                }
            "#,
        );

        let styled = style::style_tree(&document, root, &[stylesheet]);
        assert_eq!(
            styled.value("color"),
            Some(&Value::Color(Color {
                r: 0x7e,
                g: 0x28,
                b: 0x82,
                a: 255,
            }))
        );
    }

    #[test]
    fn var_reference_falls_back_when_custom_property_is_missing() {
        // No `--accent` declaration anywhere in scope — the fallback wins.
        let (document, root) = parse_html(r#"<div>x</div>"#);
        let stylesheet = parse_css(r#"div { color: var(--accent, #00ff00); }"#);

        let styled = style::style_tree(&document, root, &[stylesheet]);
        assert_eq!(
            styled.value("color"),
            Some(&Value::Color(Color {
                r: 0,
                g: 255,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn var_reference_resolves_against_inherited_custom_property_from_ancestor() {
        // `--accent` is defined on the outer `<div>` and consumed inside
        // the nested `<span>` — proves custom properties inherit and the
        // var-resolve pass on the child sees the merged map.
        let (document, root) = parse_html(r#"<div class="root"><span>x</span></div>"#);
        let stylesheet = parse_css(
            r#"
                .root { --accent: #ff0000; }
                span  { color: var(--accent); }
            "#,
        );

        let styled = style::style_tree(&document, root, &[stylesheet]);
        let span = &styled.children[0];
        assert_eq!(
            span.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn var_reference_local_definition_shadows_inherited_value() {
        // Child redefines `--accent`; its `color: var(--accent)` should
        // pick up the local value, not the ancestor's.
        let (document, root) = parse_html(r#"<div class="root"><span class="leaf">x</span></div>"#);
        let stylesheet = parse_css(
            r#"
                .root { --accent: #ff0000; }
                .leaf { --accent: #0000ff; color: var(--accent); }
            "#,
        );

        let styled = style::style_tree(&document, root, &[stylesheet]);
        let span = &styled.children[0];
        assert_eq!(
            span.value("color"),
            Some(&Value::Color(Color {
                r: 0,
                g: 0,
                b: 255,
                a: 255,
            }))
        );
    }

    #[test]
    fn var_reference_through_chained_custom_properties() {
        // `--primary` resolves to another var; the resolve loop should
        // chase it to the concrete color.
        let (document, root) = parse_html(r#"<div class="card">x</div>"#);
        let stylesheet = parse_css(
            r#"
                .card {
                    --base: #112233;
                    --primary: var(--base);
                    color: var(--primary);
                }
            "#,
        );

        let styled = style::style_tree(&document, root, &[stylesheet]);
        assert_eq!(
            styled.value("color"),
            Some(&Value::Color(Color {
                r: 0x11,
                g: 0x22,
                b: 0x33,
                a: 255,
            }))
        );
    }

    #[test]
    fn var_reference_cycle_collapses_to_initial_keyword() {
        // `--a` → `--b` → `--a` would loop forever without cycle protection.
        // The resolve pass should bail out at the first revisited name and
        // produce `Keyword("initial")` instead of recursing.
        let (document, root) = parse_html(r#"<div class="card">x</div>"#);
        let stylesheet = parse_css(
            r#"
                .card {
                    --a: var(--b);
                    --b: var(--a);
                    color: var(--a);
                }
            "#,
        );

        let styled = style::style_tree(&document, root, &[stylesheet]);
        assert_eq!(
            styled.value("color"),
            Some(&Value::Keyword("initial".into()))
        );
    }
}
