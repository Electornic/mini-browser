use std::collections::BTreeMap;

use crate::{
    css::{
        Combinator, Declaration, PseudoClass, Selector, SimpleSelector, SimpleSelectorKind,
        Stylesheet, TrackSize, Unit, Value,
    },
    dom::{Document, ElementData, NodeId, NodeType},
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

/// Per-node pseudo-class state used during selector matching.
#[derive(Default, Copy, Clone, Debug)]
struct PseudoState {
    hover: bool,
    focus: bool,
    active: bool,
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
    // The root font-size feeds rem resolution for every descendant. Compute it up front
    // by treating the root as if it lived inside the user-agent default font-size.
    let raw_root = specified_values(document, root, stylesheets, &[], PseudoState::default());
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
        &[],
        &[],
        state,
    )
}

fn pseudo_state_for(state: InteractionState<'_>, current_path: &[usize]) -> PseudoState {
    let prefix_match = |target: Option<&[usize]>| -> bool {
        // Hover/active propagate up: every ancestor on the way down to the deepest
        // engaged node also enters the state. starts_with returns true when the engaged
        // path begins with `current_path`, i.e. current is an ancestor (or self).
        matches!(target, Some(p) if p.starts_with(current_path))
    };
    let exact_match = |target: Option<&[usize]>| -> bool {
        // :focus only matches the focused element itself; ancestors do not pick it up.
        target == Some(current_path)
    };
    PseudoState {
        hover: prefix_match(state.hover),
        focus: exact_match(state.focus),
        active: prefix_match(state.active),
    }
}

#[allow(clippy::too_many_arguments)]
fn style_tree_inner(
    document: &Document,
    node_id: NodeId,
    stylesheets: &[Stylesheet],
    parent_values: Option<&PropertyMap>,
    root_font_size: f32,
    ancestors: &[(NodeId, PseudoState)],
    current_path: &[usize],
    state: InteractionState<'_>,
) -> StyledNode {
    let pseudo = pseudo_state_for(state, current_path);
    let mut specified_values = specified_values(document, node_id, stylesheets, ancestors, pseudo);

    // Real browsers inherit many properties. Here we only inherit a few text-related ones
    // because they make documents readable without making the style system much more complex.
    for property in [
        "color",
        "font-size",
        "text-align",
        "line-height",
        "text-shadow",
    ] {
        if !specified_values.contains_key(property)
            && let Some(value) = parent_values.and_then(|values| values.get(property))
        {
            specified_values.insert(property.to_string(), value.clone());
        }
    }

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
    // value, then resolve every other em/rem to Px in place. Percent stays untouched —
    // it depends on layout-time containing-block dimensions.
    specified_values.insert("font-size".into(), Value::Length(own_font_size, Unit::Px));
    for value in specified_values.values_mut() {
        match value {
            Value::Length(v, Unit::Em) => *value = Value::Length(*v * own_font_size, Unit::Px),
            Value::Length(v, Unit::Rem) => *value = Value::Length(*v * root_font_size, Unit::Px),
            // Track lists (grid-template-columns/rows) can mix length tracks
            // with fr tracks; resolve em/rem inside Length tracks the same way
            // we resolve top-level lengths so layout only ever sees Px / %.
            Value::TrackList(tracks) => {
                for track in tracks.iter_mut() {
                    match track {
                        TrackSize::Length(v, Unit::Em) => {
                            *track = TrackSize::Length(*v * own_font_size, Unit::Px);
                        }
                        TrackSize::Length(v, Unit::Rem) => {
                            *track = TrackSize::Length(*v * root_font_size, Unit::Px);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // Append self to the ancestor chain children see during their selector matching,
    // carrying the resolved pseudo-state so descendant matches can check pseudo classes
    // anchored on engaged ancestors (e.g. `.outer:hover .inner`).
    let mut child_ancestors: Vec<(NodeId, PseudoState)> = ancestors.to_vec();
    child_ancestors.push((node_id, pseudo));

    let node_data = document
        .get(node_id)
        .expect("style_tree_inner called with invalid NodeId");
    let children = node_data
        .children
        .iter()
        .enumerate()
        .map(|(idx, child_id)| {
            let mut child_path: Vec<usize> = current_path.to_vec();
            child_path.push(idx);
            style_tree_inner(
                document,
                *child_id,
                stylesheets,
                Some(&specified_values),
                root_font_size,
                &child_ancestors,
                &child_path,
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
        // CSS spec resolves font-size: <percent> against the parent's font-size, just like em.
        Some(Value::Length(v, Unit::Percent)) => *v / 100.0 * parent_font_size,
        _ => parent_font_size,
    }
}

fn specified_values(
    document: &Document,
    node_id: NodeId,
    stylesheets: &[Stylesheet],
    ancestors: &[(NodeId, PseudoState)],
    pseudo: PseudoState,
) -> PropertyMap {
    let mut matched = Vec::new();

    // First collect every rule that matches this node together with its specificity and order.
    for (rule_order, rule) in stylesheets
        .iter()
        .flat_map(|sheet| sheet.rules.iter())
        .enumerate()
    {
        if let Some(specificity) =
            matching_specificity(document, node_id, pseudo, ancestors, &rule.selectors)
        {
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
    if let Some(NodeType::Element(element)) = document.get(node_id).map(|n| &n.node_type) {
        for (name, value) in presentational_hints(element) {
            values.insert(name, value);
        }
    }
    for (_, _, declarations) in matched {
        apply_declarations(&mut values, declarations);
    }

    values
}

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

fn matching_specificity(
    document: &Document,
    node_id: NodeId,
    pseudo: PseudoState,
    ancestors: &[(NodeId, PseudoState)],
    selectors: &[Selector],
) -> Option<u32> {
    // The highest matching selector wins within a rule group such as `h1, .title`.
    selectors
        .iter()
        .filter(|selector| matches_selector(document, node_id, pseudo, ancestors, selector))
        .map(selector_specificity)
        .max()
}

fn selector_specificity(selector: &Selector) -> u32 {
    // Sum specificity across the whole chain so descendant selectors will already give the
    // right answer once the parser starts emitting them.
    selector.parts.iter().map(simple_specificity).sum()
}

fn simple_specificity(simple: &SimpleSelector) -> u32 {
    let kind_specificity = match &simple.kind {
        SimpleSelectorKind::Tag(_) => 1,
        SimpleSelectorKind::Class(_) => 10,
        SimpleSelectorKind::Id(_) => 100,
    };
    // Each pseudo-class adds 10 (CSS spec aligns it with class specificity).
    let pseudo_specificity = if simple.pseudo.is_some() { 10 } else { 0 };
    kind_specificity + pseudo_specificity
}

fn matches_selector(
    document: &Document,
    node_id: NodeId,
    pseudo: PseudoState,
    ancestors: &[(NodeId, PseudoState)],
    selector: &Selector,
) -> bool {
    // Right-to-left matching: the rightmost simple selector is the target and must match
    // the element being styled. Each preceding part is checked against ancestors using the
    // combinator that connects it to the part on its right:
    //   - Descendant: walk up until any ancestor matches.
    //   - Child: the very next ancestor must match; no skipping.
    // Pseudo state for both the target and each ancestor is carried alongside the node so
    // pseudo-classes anchored anywhere on the chain (e.g. `.outer:hover .inner`) work.
    let Some((target, leading)) = selector.parts.split_last() else {
        return false;
    };
    if !matches_simple(document, node_id, pseudo, target) {
        return false;
    }

    let mut ancestor_iter = ancestors.iter().rev();
    for (j, part) in leading.iter().enumerate().rev() {
        let combinator = selector.combinators[j];
        match combinator {
            Combinator::Descendant => loop {
                match ancestor_iter.next() {
                    Some((ancestor_id, ancestor_pseudo))
                        if matches_simple(document, *ancestor_id, *ancestor_pseudo, part) =>
                    {
                        break;
                    }
                    Some(_) => continue,
                    None => return false,
                }
            },
            Combinator::Child => match ancestor_iter.next() {
                Some((ancestor_id, ancestor_pseudo))
                    if matches_simple(document, *ancestor_id, *ancestor_pseudo, part) => {}
                _ => return false,
            },
        }
    }
    true
}

fn matches_simple(
    document: &Document,
    node_id: NodeId,
    pseudo: PseudoState,
    simple: &SimpleSelector,
) -> bool {
    let element = match document.get(node_id).map(|n| &n.node_type) {
        Some(NodeType::Element(element)) => element,
        // Text nodes never match selectors directly; they only inherit style from parents.
        _ => return false,
    };

    let kind_match = match &simple.kind {
        SimpleSelectorKind::Tag(tag_name) => element.tag_name == *tag_name,
        SimpleSelectorKind::Class(class_name) => has_class(element, class_name),
        SimpleSelectorKind::Id(id) => element
            .attributes
            .get("id")
            .is_some_and(|value| value == id),
    };

    if !kind_match {
        return false;
    }

    match simple.pseudo {
        None => true,
        Some(PseudoClass::Hover) => pseudo.hover,
        Some(PseudoClass::Focus) => pseudo.focus,
        Some(PseudoClass::Active) => pseudo.active,
    }
}

fn has_class(element: &ElementData, class_name: &str) -> bool {
    element
        .attributes
        .get("class")
        .is_some_and(|value| value.split_whitespace().any(|class| class == class_name))
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
}
