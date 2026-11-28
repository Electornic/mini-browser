mod cascade;
mod defaults;
mod presentational;
mod variables;

use cascade::specified_values;
use variables::{CustomScope, VarMemo, resolve_var_references};

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use crate::{
    css::{
        Stylesheet, TrackBreadth, TrackList, TrackSize, Unit, Value,
        calc::{Calc, Units},
        media::{self, MediaContext},
    },
    dom::{Document, NodeId, NodeType},
    dom_select::MatchingState,
};

pub type PropertyMap = BTreeMap<String, Value>;

// Mirrors a real browser's user-agent default for fonts. Used as the baseline whenever
// no font-size is in scope (root with no font-size declaration, em/rem on the root, etc.).
const DEFAULT_FONT_SIZE: f32 = 16.0;

/// The largest magnitude a computed length, percentage or number can have —
/// Blink's `LayoutUnit` range (`INT_MAX / 64` ≈ 3.3e7 px). Page CSS can write
/// `1e39px` (f32 `inf` once parsed) or compound `font-size: 1000%` past f32;
/// the cascade saturates here so layout only ever adds finite values.
pub const MAX_CSS_LENGTH: f32 = 33_554_431.0;

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

    /// Computed `visibility` is `hidden` or `collapse` (compat round 4
    /// step 1a — inherited, so a text leaf answers for itself). The box
    /// keeps its space; paint, hit testing, links, Tab and the agent's
    /// observation and actions skip it. `collapse` on table rows/columns is
    /// approximated as `hidden`.
    pub fn visibility_hidden(&self) -> bool {
        matches!(self.value("visibility"), Some(Value::Keyword(k))
            if k.eq_ignore_ascii_case("hidden") || k.eq_ignore_ascii_case("collapse"))
    }

    /// Computed `pointer-events` is `none` (compat round 4 step 1b —
    /// inherited): the hit test passes through the box to what is under
    /// it; an `auto` descendant takes clicks again. HTML has only `auto`
    /// and `none`; the SVG values count as `auto`.
    pub fn pointer_events_none(&self) -> bool {
        matches!(self.value("pointer-events"), Some(Value::Keyword(k)) if k.eq_ignore_ascii_case("none"))
    }

    /// Inside an `inert` subtree (HTML §6.3; Blink keeps this as a computed
    /// style bit too). Painted as usual, but not hit-testable (as if
    /// `pointer-events: none`), not focusable, and not offered to the agent.
    pub fn inert(&self) -> bool {
        self.specified_values.contains_key(INERT_FLAG)
    }
}

/// The internal computed-value key behind [`StyledNode::inert`]. Set from
/// the `inert` attribute and inherited by the style pass itself — never by
/// a declaration (an author `-internal-inert` is dropped) and not in
/// `INHERITED_PROPERTIES`, so `all`/`unset` can't clear it.
const INERT_FLAG: &str = "-internal-inert";

/// User-facing interaction state passed into `style_tree_with_state`. Each field is the
/// DOM path (sequence of child indices from the root) of the corresponding node, or
/// `None` if the state is not active.
#[derive(Default, Copy, Clone, Debug)]
pub struct InteractionState<'a> {
    pub hover: Option<&'a [usize]>,
    pub focus: Option<&'a [usize]>,
    pub active: Option<&'a [usize]>,
    /// The focus got there by keyboard (`:focus-visible`, compat round 3
    /// step 4b — `BrowserState` records the cause of each focus move).
    pub focus_visible: bool,
}

pub fn style_tree(document: &Document, root: NodeId, stylesheets: &[Stylesheet]) -> StyledNode {
    // Most callers do not care about interaction state — they get a "nothing engaged" tree,
    // with `@media` evaluated for the agent's default 1280×800 viewport.
    style_tree_with_state(
        document,
        root,
        stylesheets,
        InteractionState::default(),
        &MediaContext::default(),
    )
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
        &MediaContext::default(),
    )
}

/// Build the styled tree given a complete picture of interaction state. Each path slice
/// identifies the node under that interaction; matching uses CSS-spec semantics —
/// :hover and :active propagate to ancestors of the deepest engaged node, while :focus
/// only matches the focused node itself.
///
/// `media` is the viewport `@media` blocks are evaluated against — once,
/// here, for the whole pass (docs/compat-round3.md 2.1), never per node.
pub fn style_tree_with_state(
    document: &Document,
    root: NodeId,
    stylesheets: &[Stylesheet],
    state: InteractionState<'_>,
    media: &MediaContext,
) -> StyledNode {
    style_pass(document, root, stylesheets, state, media, None).0
}

/// [`style_tree_with_state`] plus whether the pass read the viewport
/// height (a `vh`/`vmin`/`vmax` length, step 5a) — the browser's view
/// cache then keys on the height (`@media` covers the rest).
pub fn style_tree_reading_viewport(
    document: &Document,
    root: NodeId,
    stylesheets: &[Stylesheet],
    state: InteractionState<'_>,
    media: &MediaContext,
) -> (StyledNode, bool) {
    style_pass(document, root, stylesheets, state, media, None)
}

/// What one style pass found at computed-value time — for the compat
/// probe only (docs/compat-round3.md 1단계 "probe 변경"); the browser never
/// asks for it. Entries are [`declaration_key`]s of the *declared* value,
/// so the probe can count the stylesheet declarations they came from.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StyleReport {
    /// Declarations that were invalid at computed-value time on at least
    /// one node: a `var()` that didn't resolve (undefined with no
    /// fallback, or a cycle).
    pub iacvt: BTreeSet<String>,
    /// Declarations whose `var()` substitution left a `Keyword` the
    /// property's value kind doesn't accept (`width: var(--w)` with
    /// `--w: 100vw`) — the computed-time twin of the parse-time keyword
    /// fallback count.
    pub var_keyword_fallback: BTreeSet<String>,
    /// For entries of either set whose substitution succeeded but whose
    /// re-parse failed or was rejected: the substituted text (the first
    /// node's), so the probe can tell what the parser didn't read — a
    /// `calc()` inside a custom property (step 5 P).
    pub reparsed_text: BTreeMap<String, String>,
    /// For IACVT entries: the `var()` names the substitution looked up and
    /// found nothing for (undefined, or guaranteed-invalid — a cycle) on
    /// the first node it failed at. The probe calls the declaration
    /// browser-too when the page declares none of them (step 5r).
    pub missing_vars: BTreeMap<String, BTreeSet<String>>,
}

/// The identity [`StyleReport`] records a declaration under: property name
/// plus the declared (unsubstituted) value. Two identical declarations in
/// different rules share a key — the probe counts every sheet occurrence of
/// a reported key.
pub fn declaration_key(name: &str, value: &Value) -> String {
    match value {
        // A shorthand's value sits on each of its longhands (step 1c): one
        // key for all of them, so it counts as the one declaration it was.
        Value::Unparsed {
            shorthand: Some(shorthand),
            raw,
        } => format!("{shorthand}: {raw:?}"),
        Value::Unparsed {
            shorthand: None,
            raw,
        } => format!("{name}: {raw:?}"),
        _ => format!("{name}: {value:?}"),
    }
}

/// [`style_tree_with_state`] plus the pass's [`StyleReport`]. Same styled
/// tree; only the bookkeeping differs.
pub fn style_tree_with_report(
    document: &Document,
    root: NodeId,
    stylesheets: &[Stylesheet],
    state: InteractionState<'_>,
    media: &MediaContext,
) -> (StyledNode, StyleReport) {
    let report = RefCell::new(StyleReport::default());
    let (styled, _) = style_pass(document, root, stylesheets, state, media, Some(&report));
    (styled, report.into_inner())
}

fn style_pass(
    document: &Document,
    root: NodeId,
    stylesheets: &[Stylesheet],
    state: InteractionState<'_>,
    media: &MediaContext,
    report: Option<&RefCell<StyleReport>>,
) -> (StyledNode, bool) {
    let media_results: Vec<Vec<bool>> = stylesheets
        .iter()
        .map(|sheet| media::evaluate_blocks(&sheet.media, media))
        .collect();
    // One document-wide layer order across the sheets (step 2.2), once per
    // pass like the media results.
    let layer_ranks = crate::css::layer::document_layer_ranks(stylesheets);
    let var_memo = RefCell::new(VarMemo::new());
    let cascade = Cascade {
        stylesheets,
        media: &media_results,
        layer_ranks: &layer_ranks,
        report,
        var_memo: &var_memo,
        viewport: Viewport {
            vw: media.viewport_width / 100.0,
            vh: media.viewport_height / 100.0,
            read_height: Cell::new(false),
        },
    };
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
        hover: state
            .hover
            .and_then(|p| resolve_relative_path(document, root, p)),
        focus: state
            .focus
            .and_then(|p| resolve_relative_path(document, root, p)),
        active: state
            .active
            .and_then(|p| resolve_relative_path(document, root, p)),
        focus_visible: state.focus_visible,
    };
    // The root font-size feeds rem resolution for every descendant. Compute it up front
    // by treating the root as if it lived inside the user-agent default font-size.
    let raw_root = specified_values(document, root, &cascade, &matching_state);
    let root_font_size = resolve_font_size(
        raw_root.get("font-size"),
        DEFAULT_FONT_SIZE,
        DEFAULT_FONT_SIZE,
        &cascade.viewport,
    );
    let styled = style_tree_inner(
        document,
        root,
        &cascade,
        None,
        &Rc::new(CustomScope::default()),
        root_font_size,
        &matching_state,
    );
    (styled, cascade.viewport.read_height.get())
}

/// 1vw / 1vh of the page viewport in px, and whether the pass used the
/// height (step 5a).
pub(super) struct Viewport {
    vw: f32,
    vh: f32,
    read_height: Cell<bool>,
}

impl Viewport {
    /// A viewport-unit length in px (`None` for other units).
    fn px(&self, value: f32, unit: Unit) -> Option<f32> {
        let px = match unit {
            Unit::Vw => value * self.vw,
            Unit::Vh => value * self.vh,
            Unit::Vmin => value * self.vw.min(self.vh),
            Unit::Vmax => value * self.vw.max(self.vh),
            _ => return None,
        };
        if unit != Unit::Vw {
            self.read_height.set(true);
        }
        Some(px)
    }

    fn units(&self, em: f32, rem: f32) -> Units {
        Units {
            em,
            rem,
            vw: self.vw,
            vh: self.vh,
        }
    }

    /// `calc` with its font and viewport units folded (`em` = `em`).
    fn resolve(&self, calc: &Calc, em: f32, rem: f32) -> Calc {
        if calc.uses_viewport_height() {
            self.read_height.set(true);
        }
        calc.resolve(&self.units(em, rem))
    }
}

/// The author sheets of one style pass plus which of their `@media` blocks
/// match (`media[sheet][block.index]`) and their layers' document-wide
/// ranks (`layer_ranks[sheet][layer]`).
pub(super) struct Cascade<'a> {
    pub stylesheets: &'a [Stylesheet],
    pub media: &'a [Vec<bool>],
    pub layer_ranks: &'a [Vec<u32>],
    /// Only the probe's pass collects one (`style_tree_with_report`).
    pub report: Option<&'a RefCell<StyleReport>>,
    /// `var()` re-parse results for this pass (step 1c).
    pub var_memo: &'a RefCell<VarMemo>,
    /// What viewport units resolve against (step 5a).
    pub viewport: Viewport,
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
    cascade: &Cascade<'_>,
    parent_values: Option<&PropertyMap>,
    parent_custom: &Rc<CustomScope>,
    root_font_size: f32,
    state: &MatchingState,
) -> StyledNode {
    // Custom properties (`--*`) leave the property map: nothing downstream
    // reads them, they only feed `var()`. They inherit through a shared
    // scope chain instead of being copied into every node.
    let (own_custom, mut specified_values): (PropertyMap, PropertyMap) =
        specified_values(document, node_id, cascade, state)
            .into_iter()
            .partition(|(name, _)| name.starts_with("--"));
    let custom = CustomScope::child(parent_custom, own_custom);

    // Resolve `var()` references against the `--*` declarations now in scope.
    // Done before the em/rem rewrite below so a custom property that holds
    // a length (e.g. `--gap: 1em`) goes through the same em/rem conversion
    // as if it had been written inline. Cycle-protected; unresolved
    // references with no fallback degrade to `Keyword("initial")`.
    resolve_var_references(
        &mut specified_values,
        &custom,
        cascade.var_memo,
        cascade.report,
    );

    // CSS-wide keywords (step 1b): `inherit` takes the parent's computed
    // value; `initial` drops the value (the consumer's default is the
    // initial value); `unset` — and `revert`/`revert-layer`, approximated
    // as `unset` (no UA-sheet rollback) — is `inherit` for an inherited
    // property and `initial` otherwise. After `var()`, so a substituted
    // keyword resolves too; before the inheritance copy below, which must
    // not refill an inherited property explicitly set to `initial`.
    let forced_initial = resolve_css_wide_keywords(&mut specified_values, parent_values);

    // Real browsers inherit many properties. Here we only inherit a few text-related ones
    // because they make documents readable without making the style system much more complex
    // (the list and why each is on it: `INHERITED_PROPERTIES`).
    for &property in INHERITED_PROPERTIES {
        if !specified_values.contains_key(property)
            && !forced_initial.contains(&property)
            && let Some(value) = parent_values.and_then(|values| values.get(property))
        {
            specified_values.insert(property.to_string(), value.clone());
        }
    }

    // `currentcolor` (follow-up F2) is the element's `color`; on `color`
    // itself it is the parent's. Resolved here, after inheritance, so
    // paint only ever sees colours (it understood the keyword for border
    // colours only).
    resolve_current_color(&mut specified_values, parent_values);

    // A side whose border style is `none`/`hidden` has a 0 border width
    // (CSS 2.1 §8.5.3) — `border: none`, or a `border-{side}` shorthand
    // that omitted the style (step 1b). Only an explicit style counts: UA
    // defaults and `border="1"` widths carry no style key and still paint.
    for side in ["top", "right", "bottom", "left"] {
        if matches!(
            specified_values.get(&format!("border-{side}-style")),
            Some(Value::Keyword(k)) if k == "none" || k == "hidden"
        ) {
            specified_values.insert(format!("border-{side}"), Value::Length(0.0, Unit::Px));
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
    let own_font_size = saturate(resolve_font_size(
        specified_values.get("font-size"),
        parent_font_size,
        root_font_size,
        &cascade.viewport,
    ));
    // `line-height` (step 5a): a length calc is folded to px against the
    // element's own font — `%` included — so no `Calc` leaves the style
    // pass here; a unitless calc was already a `Number` (a factor that
    // inherits as such).
    if let Some(Value::Calc(calc)) = specified_values.get("line-height") {
        let px = cascade
            .viewport
            .resolve(calc, own_font_size, root_font_size)
            .eval(own_font_size);
        specified_values.insert("line-height".into(), Value::Length(saturate(px), Unit::Px));
    }

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
            Value::Length(v, unit @ (Unit::Vw | Unit::Vh | Unit::Vmin | Unit::Vmax)) => {
                let px = cascade.viewport.px(*v, *unit).unwrap_or(0.0);
                *value = Value::Length(px, Unit::Px);
            }
            // Step 5a: font and viewport units folded; what is left is
            // `px + %` for layout, or a plain px length without a `%`.
            Value::Calc(calc) => {
                let resolved = cascade
                    .viewport
                    .resolve(calc, own_font_size, root_font_size);
                *value = if resolved.has_percent() {
                    Value::Calc(Box::new(resolved))
                } else {
                    Value::Length(saturate(resolved.eval(0.0)), Unit::Px)
                };
            }
            // Track lists (grid-template-columns/rows) can mix length tracks
            // with fr tracks; resolve em/rem/ch/pt inside Length tracks the same
            // way we resolve top-level lengths so layout only ever sees Px / %.
            // `background-size` lengths (step 1b): font-relative units to px.
            Value::BackgroundSize(crate::css::BackgroundSize::Size(width, height)) => {
                for axis in [width, height] {
                    if let crate::css::SizeAxis::Length(v, unit) = axis {
                        let px = match unit {
                            Unit::Em => Some(*v * own_font_size),
                            Unit::Rem => Some(*v * root_font_size),
                            Unit::Ch => Some(*v * ch_unit),
                            Unit::Pt => Some(*v * PT_TO_PX),
                            other => cascade.viewport.px(*v, *other),
                        };
                        if let Some(px) = px {
                            *axis = crate::css::SizeAxis::Length(saturate(px), Unit::Px);
                        }
                    }
                }
            }
            // Copy on write (`Arc`, step 5b): most lists are already px/fr.
            Value::TrackList(tracks) if track_list_needs_units(tracks) => {
                let viewport = &cascade.viewport;
                let to_px = |v: f32, unit: Unit| match unit {
                    Unit::Em => (v * own_font_size, Unit::Px),
                    Unit::Rem => (v * root_font_size, Unit::Px),
                    Unit::Ch => (v * ch_unit, Unit::Px),
                    Unit::Pt => (v * PT_TO_PX, Unit::Px),
                    other => match viewport.px(v, other) {
                        Some(px) => (px, Unit::Px),
                        None => (v, other),
                    },
                };
                let breadth_to_px = |breadth: &mut TrackBreadth| {
                    if let TrackBreadth::Length(v, unit) = *breadth {
                        let (v, unit) = to_px(v, unit);
                        *breadth = TrackBreadth::Length(v, unit);
                    }
                };
                let list = std::sync::Arc::make_mut(tracks);
                let auto = list
                    .auto_repeat
                    .iter_mut()
                    .flat_map(|a| a.tracks.iter_mut());
                for track in list.tracks.iter_mut().chain(auto) {
                    match track {
                        TrackSize::Length(v, unit) => {
                            let (v, unit) = to_px(*v, *unit);
                            *track = TrackSize::Length(v, unit);
                        }
                        TrackSize::MinMax(min, max) => {
                            breadth_to_px(min);
                            breadth_to_px(max);
                        }
                        TrackSize::FitContent(limit) => breadth_to_px(limit),
                        _ => {}
                    }
                }
                // Step 5b: a track calc keeps only `px + %` for layout.
                for calc in list.calcs.iter_mut() {
                    *calc = viewport.resolve(calc, own_font_size, root_font_size);
                }
            }
            _ => {}
        }
        saturate_value(value);
    }

    // The selectors crate walks the live DOM via the `Element` trait
    // implementation in `dom_select.rs`, so we no longer need to thread an
    // explicit ancestor chain through the recursion.
    // Arena invariant: children lists only hold live ids — every removal
    // path (remove_child, replace_with_text, innerHTML, replace_child)
    // detaches before tombstoning. The root comes from the caller.
    let node_data = document
        .get(node_id)
        .expect("style_tree_inner called with invalid NodeId");
    // `inert` (step 1a): from the attribute or the parent, nothing else.
    specified_values.remove(INERT_FLAG);
    let own_inert = matches!(&node_data.node_type,
        NodeType::Element(element) if element.attributes.contains_key("inert"));
    if own_inert || parent_values.is_some_and(|values| values.contains_key(INERT_FLAG)) {
        specified_values.insert(INERT_FLAG.to_string(), Value::Keyword("inert".into()));
    }
    // An inline `<svg>` is replaced content (follow-up F1): the engine
    // doesn't render SVG, and its `<title>`/`<text>`/shapes must not lay
    // out as page boxes — layout sizes the `<svg>` box from its attributes.
    let replaced = matches!(&node_data.node_type,
        NodeType::Element(element) if element.tag_name == "svg");
    let children = node_data
        .children
        .iter()
        .filter(|_| !replaced)
        .map(|child_id| {
            style_tree_inner(
                document,
                *child_id,
                cascade,
                Some(&specified_values),
                &custom,
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

/// The properties this engine inherits (CSS defines more; these are the
/// ones layout and paint read from a text leaf or rely on cascading down).
pub(super) const INHERITED_PROPERTIES: &[&str] = &[
    "color",
    "font-size",
    "text-align",
    "line-height",
    "text-shadow",
    // white-space inherits per CSS spec; the layout/render whitespace
    // collapse helper consults this on the text node, which is always a
    // child of an element — without inheritance the text wouldn't see
    // `<pre>`'s `white-space: pre` declaration.
    "white-space",
    // Without it, the UA defaults that put `<pre>` / `<code>` / `<kbd>` /
    // `<samp>` / `<tt>` into a monospace family would never reach the text
    // child the renderer actually shapes.
    "font-family",
    // Same for font-weight: `<b>`, `<strong>`, `<th>`, headings are bold in
    // the UA defaults, but the renderer shapes the *text* child.
    "font-weight",
    // Compat round 4 step 1a: a `visibility: hidden` menu hides its links
    // and text; a `visible` descendant shows again. (`<col>`'s value never
    // reaches cells — they aren't its DOM children.)
    "visibility",
    // Step 1b: a `pointer-events: none` decoration's text and children
    // pass clicks through too.
    "pointer-events",
];

/// Replace `currentcolor` keywords with the computed `color` (see the call).
fn resolve_current_color(values: &mut PropertyMap, parent_values: Option<&PropertyMap>) {
    let is_current = |v: &Value| matches!(v, Value::Keyword(k) if k == "currentcolor");
    if values.get("color").is_some_and(is_current) {
        match parent_values.and_then(|p| p.get("color")).cloned() {
            Some(parent) => {
                values.insert("color".into(), parent);
            }
            None => {
                values.remove("color");
            }
        }
    }
    let Some(own) = values.get("color").cloned() else {
        return;
    };
    for (name, value) in values.iter_mut() {
        if name != "color" && is_current(value) {
            *value = own.clone();
        }
    }
}

/// Resolve `inherit` / `initial` / `unset` / `revert` / `revert-layer` in
/// place (see the call site). Returns the inherited properties that became
/// `initial`, which the inheritance copy must leave alone.
fn resolve_css_wide_keywords(
    values: &mut PropertyMap,
    parent: Option<&PropertyMap>,
) -> Vec<&'static str> {
    let mut forced_initial = Vec::new();
    let keyworded: Vec<(String, String)> = values
        .iter()
        .filter_map(|(name, value)| match value {
            Value::Keyword(k)
                if crate::css::properties::CSS_WIDE_KEYWORDS.contains(&k.as_str()) =>
            {
                Some((name.clone(), k.clone()))
            }
            _ => None,
        })
        .collect();
    for (name, keyword) in keyworded {
        let inherited = INHERITED_PROPERTIES.iter().find(|p| **p == name).copied();
        let from_parent = keyword == "inherit" || (inherited.is_some() && keyword != "initial");
        match parent.and_then(|p| p.get(&name)).filter(|_| from_parent) {
            Some(value) => {
                values.insert(name, value.clone());
            }
            None => {
                values.remove(&name);
                if let (Some(property), false) = (inherited, from_parent) {
                    forced_initial.push(property);
                }
            }
        }
    }
    forced_initial
}

/// Clamp a computed number into the range layout can add up without
/// overflowing f32 (see [`MAX_CSS_LENGTH`]); NaN — which no parser should
/// produce — becomes 0.
fn saturate(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(-MAX_CSS_LENGTH, MAX_CSS_LENGTH)
    }
}

/// Saturate every number layout reads from a computed value: lengths and
/// percentages, bare numbers (`flex-grow`, `line-height`), grid track sizes.
/// Paint-only values (shadows, transforms, gradients) keep what they parsed.
fn saturate_value(value: &mut Value) {
    match value {
        Value::Length(v, _) | Value::Number(v) => *v = saturate(*v),
        Value::TrackList(tracks) if track_list_has_wild_numbers(tracks) => {
            let list = std::sync::Arc::make_mut(tracks);
            let saturate_breadth = |breadth: &mut TrackBreadth| {
                if let TrackBreadth::Length(v, _) | TrackBreadth::Fraction(v) = breadth {
                    *v = saturate(*v);
                }
            };
            let auto = list
                .auto_repeat
                .iter_mut()
                .flat_map(|a| a.tracks.iter_mut());
            for track in list.tracks.iter_mut().chain(auto) {
                match track {
                    TrackSize::Length(v, _) | TrackSize::Fraction(v) => *v = saturate(*v),
                    TrackSize::MinMax(min, max) => {
                        saturate_breadth(min);
                        saturate_breadth(max);
                    }
                    TrackSize::FitContent(limit) => saturate_breadth(limit),
                    TrackSize::Auto
                    | TrackSize::MinContent
                    | TrackSize::MaxContent
                    | TrackSize::Calc(_) => {}
                }
            }
        }
        _ => {}
    }
}

/// The tracks of a list, the auto repeat's included.
fn all_tracks(list: &TrackList) -> impl Iterator<Item = &TrackSize> {
    let auto: &[TrackSize] = list.auto_repeat.as_ref().map_or(&[], |a| &a.tracks);
    list.tracks.iter().chain(auto)
}

/// Whether the style pass must rewrite a track list: a calc, or a length
/// in a unit other than px / `%`.
fn track_list_needs_units(list: &TrackList) -> bool {
    let relative = |unit: &Unit| !matches!(unit, Unit::Px | Unit::Percent);
    let breadth = |b: &TrackBreadth| matches!(b, TrackBreadth::Length(_, unit) if relative(unit));
    !list.calcs.is_empty()
        || all_tracks(list).any(|track| match track {
            TrackSize::Length(_, unit) => relative(unit),
            TrackSize::MinMax(min, max) => breadth(min) || breadth(max),
            TrackSize::FitContent(limit) => breadth(limit),
            _ => false,
        })
}

/// Whether a track list holds a number [`saturate`] would change.
fn track_list_has_wild_numbers(list: &TrackList) -> bool {
    let wild = |v: f32| v.is_nan() || !(-MAX_CSS_LENGTH..=MAX_CSS_LENGTH).contains(&v);
    let breadth = |b: &TrackBreadth| matches!(b, TrackBreadth::Length(v, _) | TrackBreadth::Fraction(v) if wild(*v));
    all_tracks(list).any(|track| match track {
        TrackSize::Length(v, _) | TrackSize::Fraction(v) => wild(*v),
        TrackSize::MinMax(min, max) => breadth(min) || breadth(max),
        TrackSize::FitContent(limit) => breadth(limit),
        _ => false,
    })
}

fn resolve_font_size(
    raw: Option<&Value>,
    parent_font_size: f32,
    root_font_size: f32,
    viewport: &Viewport,
) -> f32 {
    match raw {
        // Step 5a: `em` and `%` are the parent's font size; folded to px
        // here — the font size is every descendant's `em` basis.
        Some(Value::Calc(calc)) => viewport
            .resolve(calc, parent_font_size, root_font_size)
            .eval(parent_font_size),
        Some(Value::Length(v, unit @ (Unit::Vw | Unit::Vh | Unit::Vmin | Unit::Vmax))) => {
            viewport.px(*v, *unit).unwrap_or(parent_font_size)
        }
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
        // CSS keywords `smaller` / `larger` step relative to the parent.
        // The spec defines a scaling-factor table that varies per family;
        // browsers in practice use a single ratio close to 0.83 (= 5/6)
        // and 1.2 (= 6/5). That's the ratio Phase 6.D's `<sup>`/`<sub>` UA
        // defaults rely on — `font-size: smaller` lands the footnote
        // glyphs at ~10.6px next to a 13.3px body, matching Chrome's
        // default footnote rendering on Haskell-blog-style pages.
        Some(Value::Keyword(keyword)) => match keyword.as_str() {
            "smaller" => parent_font_size * 0.83,
            "larger" => parent_font_size * 1.2,
            _ => parent_font_size,
        },
        _ => parent_font_size,
    }
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

    fn color_at(source_css: &str, width: f32) -> Option<Value> {
        let (document, root) = parse_html(r#"<div><p class="t">x</p></div>"#);
        let sheet = parse_css(source_css);
        let styled = style::style_tree_with_state(
            &document,
            root,
            &[sheet],
            style::InteractionState::default(),
            &crate::css::media::MediaContext::new(width, 800.0),
        );
        styled.children[0].value("color").cloned()
    }

    fn rgb(r: u8, g: u8, b: u8) -> Option<Value> {
        Some(Value::Color(Color { r, g, b, a: 255 }))
    }

    #[test]
    fn visibility_inherits_and_a_visible_descendant_turns_it_back_on() {
        // Compat round 4 step 1a: `visibility` is inherited (CSS 2.1
        // §11.2); a `visible` descendant of a hidden box is shown.
        let (document, root) = parse_html(
            r#"<div class="menu"><p>hidden text</p><p class="on">shown <b>bold</b></p></div>"#,
        );
        let sheet = parse_css(".menu { visibility: hidden } .on { visibility: visible }");
        let styled = style::style_tree(&document, root, &[sheet]);
        assert!(styled.visibility_hidden());
        let hidden_p = &styled.children[0];
        assert!(hidden_p.visibility_hidden());
        assert!(
            hidden_p.children[0].visibility_hidden(),
            "the text leaf too"
        );
        let shown = &styled.children[1];
        assert!(!shown.visibility_hidden());
        assert!(!shown.children[1].children[0].visibility_hidden());
        // `collapse` hides like `hidden`.
        let (document, root) = parse_html("<div><p>x</p></div>");
        let sheet = parse_css("div { visibility: collapse }");
        let styled = style::style_tree(&document, root, &[sheet]);
        assert!(styled.children[0].visibility_hidden());
    }

    #[test]
    fn pointer_events_inherits_and_an_auto_descendant_takes_clicks_again() {
        // Compat round 4 step 1b: `pointer-events` is inherited; HTML only
        // knows `auto`/`none` — the SVG values read as `auto`.
        let (document, root) = parse_html(
            r#"<div class="deco"><p>x</p><p class="on">y</p><p class="svg">z</p></div>"#,
        );
        let sheet = parse_css(
            ".deco { pointer-events: NONE } .on { pointer-events: auto } .svg { pointer-events: visiblePainted }",
        );
        let styled = style::style_tree(&document, root, &[sheet]);
        assert!(styled.pointer_events_none());
        assert!(styled.children[0].pointer_events_none());
        assert!(
            styled.children[0].children[0].pointer_events_none(),
            "text leaf"
        );
        assert!(!styled.children[1].pointer_events_none());
        assert!(!styled.children[2].pointer_events_none());
    }

    #[test]
    fn inert_flag_follows_the_attribute_down_and_cannot_be_authored() {
        // `inert` (HTML §6.3) is a computed flag like Blink's
        // `ComputedStyle::IsInert`: the attribute's subtree, text included.
        let (document, root) =
            parse_html(r#"<div><section inert><p>x</p></section><p class="t">y</p></div>"#);
        let sheet = parse_css(".t { -internal-inert: inert } section { all: initial }");
        let styled = style::style_tree(&document, root, &[sheet]);
        assert!(!styled.inert());
        let section = &styled.children[0];
        assert!(section.inert(), "`all: initial` doesn't undo the attribute");
        assert!(section.children[0].inert());
        assert!(section.children[0].children[0].inert(), "the text leaf too");
        assert!(!styled.children[1].inert(), "a page can't declare it");
    }

    #[test]
    fn media_rule_applies_only_when_matching() {
        let css = ".t { color: #000001 }
                   @media (max-width: 600px) { .t { color: #000002 } }
                   @media print { .t { color: #000003 } }";
        assert_eq!(color_at(css, 1280.0), rgb(0, 0, 1));
        assert_eq!(color_at(css, 500.0), rgb(0, 0, 2));
    }

    #[test]
    fn nested_media_is_and() {
        let css = "@media screen { @media (max-width: 600px) { .t { color: #000002 } } }
                   @media print { @media (min-width: 1px) { .t { color: #000003 } } }";
        assert_eq!(color_at(css, 1280.0), None);
        assert_eq!(color_at(css, 500.0), rgb(0, 0, 2));
    }

    fn color_with_sheets(sheets: &[&str], html: &str) -> Option<Value> {
        let (document, root) = parse_html(html);
        let sheets: Vec<_> = sheets.iter().map(|css| parse_css(css)).collect();
        let styled = style::style_tree(&document, root, &sheets);
        styled.children[0].value("color").cloned()
    }

    fn layered_color(css: &str) -> Option<Value> {
        color_with_sheets(&[css], r#"<div><p class="t">x</p></div>"#)
    }

    #[test]
    fn layer_rules_lose_to_unlayered() {
        // Higher specificity inside a layer still loses to an unlayered rule.
        let css = "@layer a { .t.t.t { color: #000002 } } .t { color: #000001 }
                   @layer { div .t.t { color: #000003 } }";
        assert_eq!(layered_color(css), rgb(0, 0, 1));
    }

    #[test]
    fn later_layer_beats_earlier() {
        let css = "@layer a { .t.t { color: #000001 } } @layer b { .t { color: #000002 } }
                   @layer a { div .t.t { color: #000003 } }";
        assert_eq!(layered_color(css), rgb(0, 0, 2));
        // Anonymous layers are each a new layer, in order.
        let css = "@layer { .t.t { color: #000001 } } @layer { .t { color: #000002 } }";
        assert_eq!(layered_color(css), rgb(0, 0, 2));
    }

    #[test]
    fn important_in_layer_beats_unlayered_important() {
        let css =
            ".t.t { color: #000001 !important } @layer a { .t { color: #000002 !important } }";
        assert_eq!(layered_color(css), rgb(0, 0, 2));
        // Among layers the order reverses too: the earlier layer wins.
        let css = "@layer a { .t { color: #000001 !important } }
                   @layer b { .t.t { color: #000002 !important } }";
        assert_eq!(layered_color(css), rgb(0, 0, 1));
        // A normal unlayered declaration still loses to a layered important one.
        let css = "@layer a { .t { color: #000001 !important } } .t.t { color: #000002 }";
        assert_eq!(layered_color(css), rgb(0, 0, 1));
    }

    #[test]
    fn inline_style_beats_layered_rules() {
        let html = r#"<div><p class="t" style="color: #000005">x</p></div>"#;
        let css = "@layer a { .t { color: #000001 } }";
        assert_eq!(color_with_sheets(&[css], html), rgb(0, 0, 5));
        let html = r#"<div><p class="t" style="color: #000005 !important">x</p></div>"#;
        let css = "@layer a { .t { color: #000001 !important } }";
        assert_eq!(color_with_sheets(&[css], html), rgb(0, 0, 5));
    }

    #[test]
    fn dark_media_inside_layer_stays_off() {
        let css = "@layer base { .t { color: #000001 }
                     @media (prefers-color-scheme: dark) { .t { color: #000009 } } }";
        assert_eq!(layered_color(css), rgb(0, 0, 1));
    }

    #[test]
    fn parent_direct_rules_beat_child_layer() {
        let css = "@layer a { @layer b { .t { color: #ff0000 } } .t { color: #0000ff } }";
        assert_eq!(layered_color(css), rgb(0, 0, 255));
        // The same with dotted names, the child declared after the parent.
        let css = "@layer a { .t { color: #000001 } } @layer a.b { .t.t { color: #000002 } }";
        assert_eq!(layered_color(css), rgb(0, 0, 1));
        // `a.b` is the layer `b` nested in `a`: one layer, one rank.
        let css = "@layer a { @layer b { .t.t { color: #000001 } } }
                   @layer c { .t { color: #000002 } }
                   @layer a.b { div .t.t { color: #000003 } }";
        assert_eq!(layered_color(css), rgb(0, 0, 2));
    }

    #[test]
    fn layer_statement_fixes_order() {
        let css =
            "@layer b, a; @layer a { .t { color: #000001 } } @layer b { .t.t { color: #000002 } }";
        assert_eq!(layered_color(css), rgb(0, 0, 1));
        // A statement inside a layer names its sublayers.
        let css = "@layer x { @layer late, early; }
                   @layer x.early { .t { color: #000001 } }
                   @layer x.late { .t.t { color: #000002 } }";
        assert_eq!(layered_color(css), rgb(0, 0, 1));
    }

    #[test]
    fn same_layer_name_across_sheets_shares_rank() {
        // The second sheet's `a` is the first sheet's `a` (before `b`),
        // not a new last layer.
        let html = r#"<div><p class="t">x</p></div>"#;
        let first = "@layer a { } @layer b { .t { color: #000002 } }";
        let second = "@layer a { .t.t { color: #000003 } }";
        assert_eq!(color_with_sheets(&[first, second], html), rgb(0, 0, 2));
        // A layer first seen in the second sheet comes after every layer
        // of the first.
        let second = "@layer c { .t { color: #000004 } }";
        assert_eq!(color_with_sheets(&[first, second], html), rgb(0, 0, 4));
    }

    #[test]
    fn layer_first_seen_in_false_media_still_takes_its_place() {
        // Approximation (docs/compat-round3.md 2.2): a layer named in a
        // block whose condition is false is ordered anyway, by first
        // appearance — `b` is before `a` here.
        let css = "@media print { @layer b { } }
                   @layer a { .t { color: #000001 } } @layer b { .t.t { color: #000002 } }";
        assert_eq!(layered_color(css), rgb(0, 0, 1));
    }

    #[test]
    fn invalid_layer_preludes_drop_the_block() {
        let css = ".t { color: #000001 }
                   @layer a b { .t { color: #000009 } }
                   @layer a, b { .t { color: #000009 } }
                   @layer a. { .t { color: #000009 } }
                   @layer 1x { .t { color: #000009 } }";
        assert_eq!(layered_color(css), rgb(0, 0, 1));
    }

    // ---- F4: selector rule index ----

    /// The indexed cascade and the flat one (every selector tried on every
    /// node — the cascade before F4) must agree exactly.
    fn assert_index_matches_flat(html: &str, css: &str) {
        let document = html::parse(html).unwrap();
        let root = *document.roots().last().unwrap();
        let sheet = parse_css(css);
        let flat = sheet.clone().with_flat_index();
        assert_ne!(sheet.index, flat.index, "the real index buckets something");
        let indexed = style::style_tree(&document, root, std::slice::from_ref(&sheet));
        let reference = style::style_tree(&document, root, &[flat]);
        assert_eq!(indexed, reference, "{css}");
    }

    #[test]
    fn rule_index_matches_the_flat_cascade() {
        let html = r##"<!DOCTYPE html><html><body>
            <div id="x" class="a  b	c"><p class="b">t <span id="X" class="B">u</span></p>
            <svg class="a"><foreignObject class="f"></foreignObject></svg>
            <a id="two words" class="c" title="t" href="#">l</a></div></body></html>"##;
        let css = "
            #x { color: #000001 } #X { color: #000002 } #two { color: #000003 }
            .a { margin-top: 1px } .b { margin-top: 2px } .B { margin-top: 3px }
            div.c { padding-top: 1px } DIV { padding-left: 2px } p .b, span#X { padding-right: 3px }
            * { line-height: 2 } [title] { width: 7px } a:not(.q) { height: 4px }
            :not(.a) > span { font-size: 9px } .a.b.c { opacity: 0.5 }
            foreignobject { width: 1px } .f { height: 2px }
            p, .nope, #nope { border-top-width: 5px }
            .b { color: #000004 !important } @layer l { .c { color: #000005 !important } }
            @media print { .a { color: #000009 } }";
        assert_index_matches_flat(html, css);
    }

    #[test]
    fn rule_index_keeps_source_order_and_specificity_ties() {
        // Same specificity in different buckets: source order still decides.
        let html = r#"<div><p id="i" class="k">x</p></div>"#;
        for css in [
            ".k { color: #000001 } p { color: #000002 } * { color: #000003 }",
            "p.k { color: #000001 } #i { color: #000002 } .k { color: #000003 }",
            "* { color: #000003 } .k, p, #i { color: #000001 }",
        ] {
            assert_index_matches_flat(html, css);
        }
    }

    // ---- compat round 3 step 4a: `:is()`/`:where()` and pseudo-elements ----

    #[test]
    fn is_and_where_match() {
        let html = r#"<div id="x" class="a"><p class="t">x</p></div>"#;
        assert_eq!(
            color_with_sheets(&[":is(.b, .a) p { color: #000001 }"], html),
            rgb(0, 0, 1)
        );
        assert_eq!(
            color_with_sheets(&[":where(#x) p { color: #000001 }"], html),
            rgb(0, 0, 1)
        );
        // `:where()` adds nothing: `:where(#x) p` ties with `p`, so the
        // later one wins either way round.
        assert_eq!(
            color_with_sheets(
                &[":where(#x) p { color: #000001 } p { color: #000002 }"],
                html
            ),
            rgb(0, 0, 2)
        );
        assert_eq!(
            color_with_sheets(
                &["p { color: #000002 } :where(#x) p { color: #000001 }"],
                html
            ),
            rgb(0, 0, 1)
        );
        // `:is()` takes its most specific argument: `:is(#x, .q) p` beats `p.t`.
        assert_eq!(
            color_with_sheets(
                &[":is(#x, .q) p { color: #000001 } p.t { color: #000002 }"],
                html
            ),
            rgb(0, 0, 1)
        );
        assert_eq!(
            color_with_sheets(&["p:is(.nope, .other) { color: #000009 }"], html),
            None
        );
    }

    #[test]
    fn is_forgiving_keeps_valid_arguments() {
        let html = r#"<div class="a"><p class="t">x</p></div>"#;
        assert_eq!(
            color_with_sheets(&[":is(.a, :unknown) p { color: #000001 }"], html),
            rgb(0, 0, 1)
        );
        // Every argument invalid: kept, matches nothing (and the comma
        // sibling still works).
        assert_eq!(
            color_with_sheets(&[":where(:frob, ::before) p, .q { color: #000009 }"], html),
            None
        );
        assert_eq!(
            color_with_sheets(&[":where(:frob) p, .t { color: #000002 }"], html),
            rgb(0, 0, 2)
        );
    }

    #[test]
    fn webkit_pseudo_with_hover_keeps_rule() {
        let css = "::-webkit-scrollbar-thumb:hover, .t { color: #000001 }
                   input::-webkit-search-cancel-button, .t { background-color: #000002 }";
        let (document, root) = parse_html(r#"<div><p class="t">x</p></div>"#);
        let styled = style::style_tree(&document, root, &[parse_css(css)]);
        let t = &styled.children[0];
        assert_eq!(t.value("color").cloned(), rgb(0, 0, 1));
        assert_eq!(t.value("background-color").cloned(), rgb(0, 0, 2));
    }

    #[test]
    fn unsupported_pseudo_still_drops_rule() {
        // `:has()` stays out of step 4: the whole rule, comma siblings
        // included, is dropped (as before).
        assert_eq!(layered_color(".t:has(span), .t { color: #000009 }"), None);
        let (_, report) = crate::css::parse_with_report(".t:has(span), .t { color: red }");
        assert_eq!(report.dropped_rules.len(), 1);
    }

    #[test]
    fn pseudo_element_in_list_keeps_sibling_selectors() {
        for pseudo in [
            "a::before",
            "a::after",
            "a:before",
            "a:after",
            "input::placeholder",
            "li::marker",
            "::selection",
            "p::first-line",
            "p:first-letter",
            "dialog::backdrop",
            "input::file-selector-button",
            "::-webkit-scrollbar",
        ] {
            let css = format!("{pseudo}, .t {{ color: #ff0000 }}");
            assert_eq!(layered_color(&css), rgb(255, 0, 0), "{css}");
        }
    }

    #[test]
    fn pseudo_element_never_matches_element() {
        for css in [
            ".t::before { color: #000009 }",
            "p::after { color: #000009 }",
            "*::-webkit-scrollbar { color: #000009 }",
            ".t::first-line { color: #000009 }",
            "::selection { color: #000009 }",
        ] {
            assert_eq!(layered_color(css), None, "{css}");
        }
        let sheet = parse_css(".t::before { color: red }");
        assert_eq!(sheet.rules.len(), 1, "kept, not dropped");
    }

    #[test]
    fn moz_pseudo_element_still_drops_rule() {
        // Chrome drops these too (browser-too): only `-webkit-` is legacy-valid.
        assert_eq!(
            layered_color("a::-moz-selection, .t { color: #000009 }"),
            None
        );
        let (_, report) = crate::css::parse_with_report("a::-moz-focus-inner, .t { color: red }");
        assert_eq!(
            report.dropped_rules[0].reason,
            crate::css::DropReason::ForeignSelector
        );
    }

    #[test]
    fn deeply_nested_is_drops_the_rule_without_overflowing() {
        // Untrusted input: the selector parser recurses per `:is(`.
        let deep = format!(
            "{}.t{} {{ color: #000009 }}",
            ":is(".repeat(20_000),
            ")".repeat(20_000)
        );
        assert_eq!(layered_color(&deep), None);
        let shallow = format!(
            "{}.t{} {{ color: #000001 }}",
            ":is(".repeat(8),
            ")".repeat(8)
        );
        assert_eq!(layered_color(&shallow), rgb(0, 0, 1));
        assert!(
            crate::css::parse_selector(&format!(
                "{}a{}",
                ":where(".repeat(20_000),
                ")".repeat(20_000)
            ))
            .is_err()
        );
    }

    #[test]
    fn combinators_stacked_through_selector_arguments_are_capped() {
        // Each `:is()`/`:where()`/`:not()` argument with a combinator inside
        // another combinator multiplies the ancestor walks (DOM depth to
        // the power of the levels — 4 levels took 20 s on a 200-deep chain).
        // Two levels are allowed (`:is(.a .b) .c`, real sites use it);
        // three drop the selector like any invalid one.
        let html = r#"<div class="a"><p class="b"><span class="t">x</span></p></div>"#;
        let color = |css: &str| {
            let (document, root) = parse_html(html);
            let styled = style::style_tree(&document, root, &[parse_css(css)]);
            styled.children[0].children[0].value("color").cloned()
        };
        assert_eq!(color(":is(.a .b) .t { color: #000001 }"), rgb(0, 0, 1));
        assert_eq!(color(".a :is(.b > .t) { color: #000001 }"), rgb(0, 0, 1));
        assert_eq!(color(":not(.q .r) .t { color: #000001 }"), rgb(0, 0, 1));
        assert_eq!(color(":is(:is(.a) .b) .t { color: #000001 }"), rgb(0, 0, 1));
        // Nesting without combinators costs nothing extra.
        assert_eq!(
            color(":is(:where(:is(:not(.q)))).t { color: #000001 }"),
            rgb(0, 0, 1)
        );
        assert_eq!(color(":is(:is(.a .b) .q) .t { color: #000009 }"), None);
        assert_eq!(color(":not(:not(.a ~ .b) .q) .t { color: #000009 }"), None);
        // Checked after parsing, so even inside a forgiving list the whole
        // rule goes (only pathological input gets here).
        assert_eq!(
            color(":is(.a, :is(:is(.a .b) .q) .z) .t, .t { color: #000009 }"),
            None
        );
        assert!(crate::css::parse_selector(":is(:is(i b) b) b").is_err());
    }

    #[test]
    fn rule_index_matches_the_flat_cascade_with_is_where_and_pseudo_elements() {
        let html = r##"<!DOCTYPE html><html><body>
            <div id="x" class="a b"><p class="t">t <span class="u">u</span></p>
            <input class="i"></div></body></html>"##;
        let css = "
            :is(.a, .zz) p { color: #000001 } .t:is(p, .zz) { margin-top: 1px }
            :where(#x) .u { color: #000002 } p:where(.t) { padding-top: 3px }
            :is(#x) > p { padding-left: 4px } :is(.t, :frob) span { width: 5px }
            a::before, .u { height: 6px } p::after { height: 9px } .i::placeholder { color: #000009 }
            ::-webkit-scrollbar-thumb:hover, .i { opacity: 0.5 } :where(*) { line-height: 2 }
            :is(span) { font-size: 11px } :is(.a .t, #nope) { border-top-width: 2px }";
        assert_index_matches_flat(html, css);
    }

    // ---- compat round 3 step 4b: state pseudo-classes ----

    /// For every element child of the root, in order: 1 if its `color` is
    /// `#000001`, 2 if `#000002`, else 0 (form controls have a UA colour).
    fn child_marks(html: &str, css: &str, state: style::InteractionState<'_>) -> Vec<u8> {
        let (document, root) = parse_html(html);
        let styled = style::style_tree_with_state(
            &document,
            root,
            &[parse_css(css)],
            state,
            &crate::css::media::MediaContext::default(),
        );
        styled
            .children
            .iter()
            .filter(|c| matches!(c.node_type, crate::dom::NodeType::Element(_)))
            .map(|c| mark(c.value("color").cloned()))
            .collect()
    }

    fn mark(value: Option<Value>) -> u8 {
        if value == rgb(0, 0, 1) {
            1
        } else if value == rgb(0, 0, 2) {
            2
        } else {
            0
        }
    }

    /// `mark` of the `color` of every element with an `id`, by id.
    fn marks_by_id(html: &str, css: &str) -> std::collections::BTreeMap<String, u8> {
        let (document, root) = parse_html(html);
        let styled = style::style_tree(&document, root, &[parse_css(css)]);
        fn walk(node: &style::StyledNode, out: &mut std::collections::BTreeMap<String, u8>) {
            if let crate::dom::NodeType::Element(e) = &node.node_type
                && let Some(id) = e.attributes.get("id")
            {
                out.insert(id.clone(), mark(node.value("color").cloned()));
            }
            node.children.iter().for_each(|c| walk(c, out));
        }
        let mut out = std::collections::BTreeMap::new();
        walk(&styled, &mut out);
        out
    }

    #[test]
    fn checked_matches_checkbox_attribute() {
        let marks = child_marks(
            r#"<div><input type="checkbox" checked><input type="checkbox"><input type="radio" checked><input type="text" checked><p checked></p><input type="CHECKBOX" checked></div>"#,
            ":checked { color: #000001 }",
            Default::default(),
        );
        assert_eq!(marks, [1, 0, 1, 0, 0, 1]);
        // The checkbox hack: `:checked ~ .menu`.
        let marks = child_marks(
            r#"<div><input type="checkbox" id="t" checked><label for="t">Menu</label><ul class="menu"></ul></div>"#,
            ".menu { color: #000002 } #t:checked ~ .menu { color: #000001 }",
            Default::default(),
        );
        assert_eq!(marks[2], 1);
    }

    #[test]
    fn select_without_selected_checks_first_option() {
        let css = "option:checked { color: #000001 }";
        let marks = |html: &str| -> Vec<u8> { marks_by_id(html, css).into_values().collect() };
        // No `selected`: the first option is the selected one.
        assert_eq!(
            marks(
                r#"<div><select><option id="a">a</option><option id="b">b</option></select></div>"#
            ),
            [1, 0]
        );
        // Inside an optgroup too.
        assert_eq!(
            marks(
                r#"<div><select><optgroup><option id="a">a</option></optgroup><option id="b">b</option></select></div>"#
            ),
            [1, 0]
        );
        // `selected` wins.
        assert_eq!(
            marks(
                r#"<div><select><option id="a">a</option><option id="b" selected>b</option></select></div>"#
            ),
            [0, 1]
        );
        // A multi-select has no default selection.
        assert_eq!(
            marks(
                r#"<div><select multiple><option id="a">a</option><option id="b">b</option></select></div>"#
            ),
            [0, 0]
        );
    }

    #[test]
    fn disabled_and_enabled_on_controls() {
        let marks = child_marks(
            r##"<div><button disabled></button><button></button><input disabled><textarea></textarea><select disabled></select><p disabled></p><a href="#"></a><fieldset disabled></fieldset></div>"##,
            ":disabled { color: #000001 } :enabled { color: #000002 }",
            Default::default(),
        );
        // `<p>` and `<a>` are not form controls: neither.
        assert_eq!(marks, [1, 2, 1, 2, 1, 0, 0, 1]);
    }

    #[test]
    fn fieldset_disabled_disables_descendants() {
        let marks = marks_by_id(
            r#"<div><fieldset disabled><p><input id="a"></p><button id="b"></button><fieldset><select id="c"></select></fieldset></fieldset><input id="d"></div>"#,
            ":disabled { color: #000001 } :enabled { color: #000002 }",
        );
        assert_eq!(marks["a"], 1);
        assert_eq!(marks["b"], 1);
        assert_eq!(marks["c"], 1, "nested fieldset inherits");
        assert_eq!(marks["d"], 2, "outside the fieldset");
    }

    #[test]
    fn focus_within_matches_ancestors() {
        // Focus the link at [1, 0]: its section and the root div have it
        // within; the sibling paragraph doesn't. (`background-color`:
        // not inherited.)
        let html = r##"<div class="r"><p class="s">x</p><section class="t"><a href="#">l</a></section></div>"##;
        let css = ":focus-within { background-color: #000001 }";
        let bg = |state: style::InteractionState<'_>| {
            let (document, root) = parse_html(html);
            let styled = style::style_tree_with_state(
                &document,
                root,
                &[parse_css(css)],
                state,
                &crate::css::media::MediaContext::default(),
            );
            let of = |n: &style::StyledNode| mark(n.value("background-color").cloned());
            [
                of(&styled),
                of(&styled.children[0]),
                of(&styled.children[1]),
                of(&styled.children[1].children[0]),
            ]
        };
        let focused = bg(style::InteractionState {
            focus: Some(&[1, 0]),
            ..Default::default()
        });
        assert_eq!(focused, [1, 0, 1, 1], "root, p, section, the focused link");
        assert_eq!(bg(Default::default()), [0, 0, 0, 0], "nothing focused");
    }

    #[test]
    fn focus_visible_follows_the_keyboard_flag_except_for_text_entry() {
        let html = r#"<div><button>b</button><input><input type="checkbox"><textarea></textarea><div contenteditable></div><div contenteditable="false"></div></div>"#;
        let css = ":focus-visible { color: #000001 }";
        let marks = |index: usize, keyboard: bool| {
            let path = [index];
            child_marks(
                html,
                css,
                style::InteractionState {
                    focus: Some(&path),
                    focus_visible: keyboard,
                    ..Default::default()
                },
            )
        };
        // Focus moved by the pointer: only text entry shows a ring.
        let pointer: Vec<u8> = (0..6).map(|i| marks(i, false)[i]).collect();
        assert_eq!(
            pointer,
            [0, 1, 0, 1, 1, 0],
            "button, input, checkbox, textarea, editable, editable=false"
        );
        // From the keyboard: always — and only on the focused element.
        let keyboard: Vec<u8> = (0..6).map(|i| marks(i, true)[i]).collect();
        assert_eq!(keyboard, [1; 6]);
        assert_eq!(marks(0, true), [1, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn currentcolor_resolves_to_the_color_property() {
        let (document, root) =
            parse_html(r#"<div class="p"><p class="t">x</p><p class="u">y</p></div>"#);
        let sheet = parse_css(
            ".p { color: #000001 }
             .t { color: #000002; background-color: currentcolor; border-top-color: currentColor }
             .u { color: currentcolor; outline-color: currentcolor }",
        );
        let styled = style::style_tree(&document, root, &[sheet]);
        let t = &styled.children[0];
        assert_eq!(t.value("background-color").cloned(), rgb(0, 0, 2));
        assert_eq!(t.value("border-top-color").cloned(), rgb(0, 0, 2));
        // `color: currentcolor` is the parent's colour.
        let u = &styled.children[1];
        assert_eq!(u.value("color").cloned(), rgb(0, 0, 1));
        assert_eq!(u.value("outline-color").cloned(), rgb(0, 0, 1));
    }

    #[test]
    fn supports_true_branch_applies() {
        let css = ".t { color: #000001 } @supports (display: grid) { .t { color: #000002 } }";
        assert_eq!(layered_color(css), rgb(0, 0, 2));
    }

    #[test]
    fn supports_not_branch_skipped_when_supported() {
        let css = ".t { color: #000001 } @supports not (display: grid) { .t { color: #000009 } }
                   @supports (frobnicate: 1) { .t { color: #000009 } }";
        assert_eq!(layered_color(css), rgb(0, 0, 1));
    }

    #[test]
    fn supports_unrendered_value_takes_the_fallback() {
        // We don't render `position: sticky`: the fallback branch applies.
        let css = "@supports (position: sticky) { .t { color: #000009 } }
                   @supports not (position: sticky) { .t { color: #000002 } }";
        assert_eq!(layered_color(css), rgb(0, 0, 2));
    }

    #[test]
    fn supports_nests_with_media_and_layers() {
        let css = "@layer a { @supports (display: grid) { .t.t { color: #000002 } } }
                   .t { color: #000001 }
                   @media print { @supports (display: grid) { .t { color: #000009 } } }
                   @supports (display: grid) { @media print { .t { color: #000009 } } }";
        assert_eq!(layered_color(css), rgb(0, 0, 1));
        // A layer first named inside a false `@supports` keeps its place
        // (the 2.2 approximation): `b` comes before `a`.
        let css = "@supports (frobnicate: 1) { @layer b { } }
                   @layer a { .t { color: #000001 } } @layer b { .t.t { color: #000002 } }";
        assert_eq!(layered_color(css), rgb(0, 0, 1));
    }

    #[test]
    fn media_does_not_change_rule_order() {
        // A skipped block must not shift the source order the cascade
        // sorts by: the later matching rule still wins.
        let css = "@media (min-width: 1px) { .t { color: #000001 } }
                   @media (max-width: 1px) { .t { color: #000009 } }
                   .t { color: #000002 }";
        assert_eq!(color_at(css, 1280.0), rgb(0, 0, 2));
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
    fn ua_hides_hidden_attribute_hidden_inputs_and_templates() {
        // Naver's search form has 13 `<input type="hidden">` that painted as
        // a grid of empty boxes; GitHub's `<template>`s laid out their inert
        // content; `[hidden]` is the UA's `display: none` too.
        let (document, root) = parse_html(
            r#"<div><input type="hidden" name="a"><input type="HIDDEN" name="b"><input name="c"><p hidden>x</p><template><p>y</p></template></div>"#,
        );
        let styled = style::style_tree(&document, root, &[parse_css(".shown { display: block }")]);
        let displays: Vec<Option<&Value>> = styled
            .children
            .iter()
            .map(|child| child.value("display"))
            .collect();
        let none = Some(&Value::Keyword("none".into()));
        assert_eq!(displays[0], none, "type=hidden");
        assert_eq!(displays[1], none, "type=HIDDEN");
        assert_ne!(displays[2], none, "a text input stays");
        assert_eq!(displays[3], none, "[hidden]");
        assert_eq!(displays[4], none, "template");

        // Author CSS still wins over the UA default.
        let (document, root) = parse_html(r#"<div><p hidden class="shown">x</p></div>"#);
        let styled = style::style_tree(&document, root, &[parse_css(".shown { display: block }")]);
        assert_eq!(
            styled.children[0].value("display"),
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
        let (document, root) = parse_html(r#"<div><input type="text" value="hello"/></div>"#);
        let styled = style::style_tree(&document, root, &[]);
        let input = &styled.children[0];

        assert_eq!(
            input.value("display"),
            Some(&Value::Keyword("inline-block".into()))
        );
        assert_eq!(input.value("width"), Some(&Value::Length(200.0, Unit::Px)));
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
        assert_eq!(styled.value("width"), Some(&Value::Length(320.0, Unit::Px)));
        // `border-color` is a shorthand for the per-side colours (step 1b),
        // which the painter prefers over the UA's single `border-color`.
        assert_eq!(
            styled.value("border-top-color"),
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
            &crate::css::media::MediaContext::default(),
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
            &crate::css::media::MediaContext::default(),
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
            &crate::css::media::MediaContext::default(),
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
        assert_eq!(
            styled.value("height"),
            Some(&Value::Length(100.0, Unit::Px))
        );

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
        assert_eq!(styled.value("float"), Some(&Value::Keyword("right".into())));

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
        let (document, root) = parse_html(r#"<center><table><tr><td>x</td></tr></table></center>"#);
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
        let (document, root) =
            parse_html(r#"<center><table><tr><td>cell</td></tr></table></center>"#);
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
        let (document, root) = parse_html(r#"<div class="subtext"><a href="/x">child</a></div>"#);
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
    fn bold_emphasis_tags_cascade_font_weight_and_inherit_to_text() {
        // Phase 6.C: <b>, <strong>, and <th> get a UA `font-weight: bold`
        // default. font-weight is now an inheritable property in the
        // cascade, so the text leaf inside the bolded element sees the
        // same Keyword("bold") and the rasteriser routes shaping through
        // cosmic-text's bold face.
        let (document, root) = parse_html(r#"<p>plain<b>bold</b></p>"#);
        let styled = style::style_tree(&document, root, &[]);

        let bold_span = &styled.children[1];
        assert_eq!(
            bold_span.value("font-weight"),
            Some(&Value::Keyword("bold".into()))
        );
        // The text node inside <b> must inherit the weight — without that
        // the rasteriser still picks the regular face for the actual
        // glyph row.
        let bold_text = &bold_span.children[0];
        assert_eq!(
            bold_text.value("font-weight"),
            Some(&Value::Keyword("bold".into()))
        );
        // And a sibling text node (direct child of <p>) must NOT pick up
        // the weight — inheritance only flows through the styled subtree.
        let plain_text = &styled.children[0];
        assert_eq!(plain_text.value("font-weight"), None);
    }

    #[test]
    fn th_cascades_bold_weight_and_centered_text() {
        // <th> diverges from <td>: same display, but UA-bold + center
        // alignment so a header cell stands out without author CSS.
        let (document, root) = parse_html(r#"<table><tr><th>Hdr</th><td>Body</td></tr></table>"#);
        let styled = style::style_tree(&document, root, &[]);

        let row = &styled.children[0].children[0];
        let th_cell = &row.children[0];
        let td_cell = &row.children[1];

        assert_eq!(
            th_cell.value("font-weight"),
            Some(&Value::Keyword("bold".into()))
        );
        assert_eq!(
            th_cell.value("text-align"),
            Some(&Value::Keyword("center".into()))
        );
        // <td> stays at the cascade default — only <th> opts in.
        assert_eq!(td_cell.value("font-weight"), None);
    }

    #[test]
    fn sup_cascades_smaller_font_and_super_alignment() {
        // Phase 6.D: <sup> picks up `font-size: smaller` (0.83×) and
        // `vertical-align: super` from the UA stylesheet so footnote
        // markers (Haskell blog `<sup>1</sup>`) shrink + raise instead
        // of squatting at full size in the middle of the line.
        let (document, root) = parse_html(r#"<p class="copy">Body<sup>1</sup></p>"#);
        let stylesheet = parse_css(r#".copy { font-size: 18px; }"#);

        let styled = style::style_tree(&document, root, &[stylesheet]);
        let sup = &styled.children[1];

        match sup.value("font-size") {
            // 18 × 0.83 = 14.94. Float compare with a small tolerance to
            // keep the test stable if the ratio gets refined.
            Some(Value::Length(v, Unit::Px)) => {
                assert!((v - 18.0 * 0.83).abs() < 1e-3, "got {v}");
            }
            other => panic!("unexpected font-size: {other:?}"),
        }
        assert_eq!(
            sup.value("vertical-align"),
            Some(&Value::Keyword("super".into()))
        );
    }

    #[test]
    fn sub_cascades_smaller_font_and_sub_alignment() {
        // Mirror of the sup test — `<sub>` gets the same shrink with the
        // opposite vertical-align, used by chemistry/equation pages.
        let (document, root) = parse_html(r#"<p class="copy">H<sub>2</sub>O</p>"#);
        let stylesheet = parse_css(r#".copy { font-size: 20px; }"#);

        let styled = style::style_tree(&document, root, &[stylesheet]);
        let sub = &styled.children[1];

        match sub.value("font-size") {
            Some(Value::Length(v, Unit::Px)) => {
                assert!((v - 20.0 * 0.83).abs() < 1e-3, "got {v}");
            }
            other => panic!("unexpected font-size: {other:?}"),
        }
        assert_eq!(
            sub.value("vertical-align"),
            Some(&Value::Keyword("sub".into()))
        );
    }

    #[test]
    fn inline_style_attribute_overrides_matched_author_rules() {
        // Phase 6.E: per the CSS spec, the `style="..."` attribute beats
        // every selector match. HN's homepage cells use inline `style="width:
        // 60px"` (and friends) to control spacing; without inline-style
        // parsing that declaration was dropped on the floor and the cell
        // fell back to the author rule's width.
        let (document, root) =
            parse_html(r#"<div class="card" style="width: 60px; color: red">x</div>"#);
        let stylesheet = parse_css(r#".card { width: 100px; color: blue; }"#);

        let styled = style::style_tree(&document, root, &[stylesheet]);

        // Inline width wins over the .card class rule.
        assert_eq!(styled.value("width"), Some(&Value::Length(60.0, Unit::Px)));
        // Inline color wins for the same reason.
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
    fn inline_style_falls_back_when_attribute_is_missing() {
        // No `style` attribute → behaviour matches what the cascade
        // produced before Phase 6.E. A regression here would mean the
        // new code is doing something even on un-styled elements.
        let (document, root) = parse_html(r#"<div class="card">x</div>"#);
        let stylesheet = parse_css(r#".card { color: green; }"#);

        let styled = style::style_tree(&document, root, &[stylesheet]);
        assert_eq!(
            styled.value("color"),
            Some(&Value::Color(Color {
                r: 0,
                g: 128,
                b: 0,
                a: 255,
            }))
        );
    }

    #[test]
    fn standalone_inline_code_gets_ua_pill_background() {
        // Phase 6.H: a bare `<code>` outside `<pre>` should arrive at
        // the painter with the GitHub-style #f6f8fa background, small
        // em-scaled padding, and a 3px border-radius. Without this the
        // inline `<code>` runs render visually identical to body text
        // on unstyled pages — barely distinguishable from a regular
        // span.
        let (document, root) = parse_html(r#"<p>plain <code>x</code></p>"#);
        let styled = style::style_tree(&document, root, &[]);
        let code = &styled.children[1];

        assert_eq!(
            code.value("background-color"),
            Some(&Value::Color(Color {
                r: 246,
                g: 248,
                b: 250,
                a: 255,
            }))
        );
        // Padding is em-based and resolves to Px during the cascade —
        // 0.3em at the inherited 16px UA default lands at 4.8px.
        assert_eq!(
            code.value("padding-left"),
            Some(&Value::Length(4.8, Unit::Px))
        );
        assert_eq!(
            code.value("border-top-left-radius"),
            Some(&Value::Length(3.0, Unit::Px))
        );
    }

    #[test]
    fn code_inside_pre_skips_pill_to_avoid_double_chrome() {
        // The `<pre>` panel already paints its own faint #f6f8fa
        // background; a nested `<code>` adding another pill would
        // double up and force redundant padding. UA suppresses the
        // pill in that case but still hands down the monospace family
        // (which the existing test already covers).
        let (document, root) = parse_html(r#"<pre><code>x = 1</code></pre>"#);
        let styled = style::style_tree(&document, root, &[]);
        // pre → code is the only child path here.
        let code = &styled.children[0];

        assert_eq!(code.value("padding-left"), None);
        assert_eq!(code.value("border-top-left-radius"), None);
        // Code still inherits the `<pre>`'s background-color through
        // the cascade, but the suppression is about the UA pill — the
        // test confirms we didn't add a *second* layer of chrome.
        // background-color comes through as the inherited pre value;
        // we just verify the pill markers (padding / radius) are absent.
    }

    #[test]
    fn hr_gets_top_border_and_gray_color_ua_defaults() {
        // Phase 6.I: bare `<hr>` should arrive at the painter with a
        // 1px top border and the historical light-gray color, plus a
        // small vertical margin so adjacent paragraphs don't kiss the
        // line. HN's footer divider and the Haskell blog's section
        // breaks both rely on these defaults.
        let (document, root) = parse_html(r#"<hr>"#);
        let styled = style::style_tree(&document, root, &[]);

        assert_eq!(
            styled.value("border-top"),
            Some(&Value::Length(1.0, Unit::Px))
        );
        assert_eq!(
            styled.value("border-color"),
            Some(&Value::Color(Color {
                r: 204,
                g: 204,
                b: 204,
                a: 255,
            }))
        );
        assert_eq!(styled.value("height"), Some(&Value::Length(0.0, Unit::Px)));
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
        // produce `Keyword("initial")` instead of recursing — which the
        // CSS-wide keyword pass then resolves to "no value" (step 1b).
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
        assert_eq!(styled.value("color"), None);
    }

    #[test]
    fn overflowing_length_literal_resolves_to_a_finite_px() {
        // `1e39` overflows f32, so cssparser hands us `inf`; `3e38px` is
        // finite but two of them already overflow when layout adds them.
        // Computed lengths saturate at Blink's LayoutUnit range instead.
        let (document, root) = parse_html(r#"<div class="card">x</div>"#);
        let stylesheet = parse_css(
            r#".card { padding-left: 1e39px; margin-top: -1e39px; width: 3e38px; height: 1e39%; flex-grow: 1e39; }"#,
        );

        let styled = style::style_tree(&document, root, &[stylesheet]);
        let max = style::MAX_CSS_LENGTH;
        assert_eq!(
            styled.value("padding-left"),
            Some(&Value::Length(max, Unit::Px))
        );
        assert_eq!(
            styled.value("margin-top"),
            Some(&Value::Length(-max, Unit::Px))
        );
        assert_eq!(styled.value("width"), Some(&Value::Length(max, Unit::Px)));
        assert_eq!(
            styled.value("height"),
            Some(&Value::Length(max, Unit::Percent))
        );
        assert_eq!(styled.value("flex-grow"), Some(&Value::Number(max)));
    }

    #[test]
    fn compounding_font_size_stays_finite() {
        // 1000% per level overflows f32 after ~40 levels (16 × 10^40), and
        // every em length below it with it.
        let mut source = String::new();
        for _ in 0..45 {
            source.push_str(r#"<div class="big">"#);
        }
        source.push_str(r#"<p class="em">x</p>"#);
        for _ in 0..45 {
            source.push_str("</div>");
        }
        let (document, root) = parse_html(&source);
        let stylesheet = parse_css(".big { font-size: 1000%; } .em { margin-left: 2em; }");

        let styled = style::style_tree(&document, root, &[stylesheet]);
        let deepest_p = find_tag(&styled, "p").expect("the <p> is styled");
        for name in ["font-size", "margin-left"] {
            match deepest_p.value(name) {
                Some(Value::Length(v, Unit::Px)) => {
                    assert!(v.is_finite() && *v <= style::MAX_CSS_LENGTH, "{name} = {v}")
                }
                other => panic!("{name} = {other:?}"),
            }
        }
    }

    /// Style pass with a computed-value-time report (docs/compat-round3.md
    /// 1단계 probe 변경), at the agent's default viewport.
    fn report_for(html_source: &str, css_source: &str) -> style::StyleReport {
        let (document, root) = parse_html(html_source);
        let sheet = parse_css(css_source);
        let (_, report) = style::style_tree_with_report(
            &document,
            root,
            std::slice::from_ref(&sheet),
            style::InteractionState::default(),
            &crate::css::media::MediaContext::default(),
        );
        report
    }

    fn sheet_key(css_source: &str, property: &str) -> String {
        let sheet = parse_css(css_source);
        let decl = sheet
            .rules
            .iter()
            .flat_map(|rule| &rule.declarations)
            .find(|decl| decl.name == property)
            .expect("declaration kept");
        style::declaration_key(&decl.name, &decl.value)
    }

    #[test]
    fn style_report_counts_unresolved_var_as_iacvt() {
        let css = "p { color: var(--undefined); width: var(--w); } div { --w: 5px; }";
        let report = report_for("<div><p>x</p></div>", css);
        assert!(
            report.iacvt.contains(&sheet_key(css, "color")),
            "{report:?}"
        );
        // Resolved on the node that uses it: not IACVT.
        assert!(
            !report.iacvt.contains(&sheet_key(css, "width")),
            "{report:?}"
        );
        assert!(report.var_keyword_fallback.is_empty(), "{report:?}");
    }

    #[test]
    fn style_report_counts_cyclic_var_as_iacvt() {
        let css = "p { --a: var(--b); --b: var(--a); margin-left: var(--a); }";
        let report = report_for("<div><p>x</p></div>", css);
        assert!(
            report.iacvt.contains(&sheet_key(css, "margin-left")),
            "{report:?}"
        );
    }

    #[test]
    fn style_report_counts_substituted_keyword_the_property_rejects() {
        // `wide` is a single-token Keyword; `width` doesn't take it.
        let css = ":root { --w: wide; --c: red; } p { width: var(--w); color: var(--c); }";
        let report = report_for("<div><p>x</p></div>", css);
        assert!(
            report
                .var_keyword_fallback
                .contains(&sheet_key(css, "width")),
            "{report:?}"
        );
        assert!(
            !report
                .var_keyword_fallback
                .contains(&sheet_key(css, "color"))
        );
        assert!(report.iacvt.is_empty(), "{report:?}");
    }

    #[test]
    fn style_report_keeps_the_text_a_failed_reparse_saw() {
        // The probe blames a computed-time failure on what was re-parsed,
        // not on `var()` (step 5 P): here a `calc()` from the custom
        // property (a length times a length — invalid in every engine).
        let css = "div { --w: calc(2px * 3px); } p { width: var(--w); color: var(--undefined); }";
        let report = report_for("<div><p>x</p></div>", css);
        let width = sheet_key(css, "width");
        assert!(report.iacvt.contains(&width), "{report:?}");
        assert_eq!(
            report.reparsed_text.get(&width).map(String::as_str),
            Some("calc(2px * 3px)")
        );
        // The substitution itself failed: nothing was re-parsed.
        let color = sheet_key(css, "color");
        assert!(report.iacvt.contains(&color), "{report:?}");
        assert!(!report.reparsed_text.contains_key(&color), "{report:?}");
    }

    #[test]
    fn style_tree_without_report_matches_with_report() {
        let css = "p { color: var(--undefined); width: var(--w); } div { --w: 5px; }";
        let (document, root) = parse_html("<div><p>x</p></div>");
        let sheet = parse_css(css);
        let plain = style::style_tree(&document, root, std::slice::from_ref(&sheet));
        let (reported, _) = style::style_tree_with_report(
            &document,
            root,
            std::slice::from_ref(&sheet),
            style::InteractionState::default(),
            &crate::css::media::MediaContext::default(),
        );
        assert_eq!(plain, reported);
    }

    /// A whole page (`<html>` root, step 1r) styled like the view does.
    fn style_page(html_source: &str, css_source: &str) -> style::StyledNode {
        let document = html::parse(html_source).unwrap();
        let root = *document.roots().last().unwrap();
        style::style_tree(&document, root, &[parse_css(css_source)])
    }

    const PAGE: &str = r#"<!DOCTYPE html><html data-theme="dark"><head><title>t</title></head><body><div><p>x</p></div></body></html>"#;

    fn styled_p(html_p: &str, css: &str) -> style::StyledNode {
        let page = format!("<!DOCTYPE html><html><body><div>{html_p}</div></body></html>");
        find_tag(&style_page(&page, css), "p").unwrap().clone()
    }

    fn px(v: f32) -> Value {
        Value::Length(v, Unit::Px)
    }

    #[test]
    fn important_beats_more_specific_normal() {
        let p = styled_p(
            r#"<p id="x" class="a">x</p>"#,
            "#x { width: 1px; } .a { width: 2px !important; } p.a { width: 3px; }",
        );
        assert_eq!(p.value("width"), Some(&px(2.0)));
    }

    #[test]
    fn important_declarations_cascade_by_specificity_then_order() {
        let p = styled_p(
            r#"<p id="x" class="a">x</p>"#,
            "#x { width: 1px !important; } .a { width: 2px !important; }              .a { height: 3px !important; } p { height: 4px !important; } .a { height: 5px !important; }",
        );
        assert_eq!(
            p.value("width"),
            Some(&px(1.0)),
            "more specific important wins"
        );
        assert_eq!(
            p.value("height"),
            Some(&px(5.0)),
            "later important of equal specificity wins"
        );
    }

    #[test]
    fn important_inline_beats_important_author() {
        let p = styled_p(
            r#"<p class="a" style="width: 9px !important; height: 8px">x</p>"#,
            ".a { width: 1px !important; height: 2px !important; } #none, p { top: 1px; }",
        );
        assert_eq!(
            p.value("width"),
            Some(&px(9.0)),
            "inline important > author important"
        );
        assert_eq!(
            p.value("height"),
            Some(&px(2.0)),
            "author important > inline normal"
        );
    }

    #[test]
    fn important_shorthand_beats_more_specific_normal_longhand() {
        let p = styled_p(
            r#"<p class="a b">x</p>"#,
            ".a.b { margin-left: 3px; } .a { margin: 1px !important; }",
        );
        assert_eq!(p.value("margin-left"), Some(&px(1.0)));
    }

    #[test]
    fn important_inside_a_non_matching_media_block_does_not_apply() {
        let p = styled_p(
            r#"<p class="a">x</p>"#,
            ".a { width: 1px; } @media (max-width: 600px) { .a { width: 2px !important; } }",
        );
        assert_eq!(
            p.value("width"),
            Some(&px(1.0)),
            "1280px viewport: the block is false"
        );
    }

    #[test]
    fn root_custom_property_reaches_body_descendants() {
        let styled = style_page(PAGE, ":root { --x: 5px; } p { margin-left: var(--x); }");
        let p = find_tag(&styled, "p").unwrap();
        assert_eq!(p.value("margin-left"), Some(&Value::Length(5.0, Unit::Px)));
    }

    #[test]
    fn html_attribute_selector_matches_the_document_element() {
        let styled = style_page(
            PAGE,
            "html[data-theme=dark] p { width: 7px; } html { height: 9px; }",
        );
        assert_eq!(
            find_tag(&styled, "p").unwrap().value("width"),
            Some(&Value::Length(7.0, Unit::Px))
        );
        assert_eq!(
            find_tag(&styled, "html").unwrap().value("height"),
            Some(&Value::Length(9.0, Unit::Px))
        );
    }

    #[test]
    fn rem_resolves_against_the_html_font_size() {
        let styled = style_page(PAGE, "html { font-size: 10px; } p { width: 2rem; }");
        assert_eq!(
            find_tag(&styled, "p").unwrap().value("width"),
            Some(&Value::Length(20.0, Unit::Px))
        );
    }

    #[test]
    fn root_font_size_does_not_override_a_body_rule() {
        // `:root` (0,1,0) only matches <html>; `body` (0,0,1) still sets the
        // body's size, and rem is still the html one — as in Chrome.
        let styled = style_page(
            PAGE,
            ":root { font-size: 10px; } body { font-size: 14px; } p { width: 1rem; }",
        );
        let p = find_tag(&styled, "p").unwrap();
        assert_eq!(p.value("font-size"), Some(&Value::Length(14.0, Unit::Px)));
        assert_eq!(p.value("width"), Some(&Value::Length(10.0, Unit::Px)));
        assert_eq!(
            find_tag(&styled, "body").unwrap().value("font-size"),
            Some(&Value::Length(14.0, Unit::Px))
        );
    }

    #[test]
    fn custom_properties_inherit_without_being_copied_into_each_node() {
        // Hundreds of `:root` tokens copied into every node made GitHub's
        // style pass blow the budget (step 1r); they live in a shared scope.
        let styled = style_page(
            PAGE,
            ":root { --x: 5px; --unused: 1px; } div { --y: 3px; } p { margin-left: var(--x); width: var(--y); }",
        );
        let p = find_tag(&styled, "p").unwrap();
        assert_eq!(p.value("margin-left"), Some(&Value::Length(5.0, Unit::Px)));
        assert_eq!(p.value("width"), Some(&Value::Length(3.0, Unit::Px)));
        for node in [&styled, find_tag(&styled, "div").unwrap(), p] {
            assert!(
                node.specified_values
                    .keys()
                    .all(|name| !name.starts_with("--")),
                "{:?}",
                node.specified_values.keys().collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn unitless_zero_through_var_is_a_px_length() {
        let styled = style_page(
            PAGE,
            ":root { --z: 0; } p { top: var(--z); line-height: var(--z); }",
        );
        let p = find_tag(&styled, "p").unwrap();
        assert_eq!(p.value("top"), Some(&Value::Length(0.0, Unit::Px)));
        assert_eq!(p.value("line-height"), Some(&Value::Number(0.0)));
    }

    #[test]
    fn border_none_zeroes_width() {
        // A side whose computed style is none/hidden has a 0 border width —
        // later `border: none` beats an earlier width, and a shorthand that
        // omits the style resets it to none (CSS 2.1 §8.5.3).
        let border = |html_class: &str, css: &str| {
            let styled = styled_p(&format!(r#"<p class="{html_class}">x</p>"#), css);
            ["top", "right", "bottom", "left"]
                .map(|side| styled.value(&format!("border-{side}")).cloned())
        };
        let zero = Some(px(0.0));
        assert_eq!(
            border("a b", ".a { border: 1px solid red; } .b { border: none; }"),
            [zero.clone(), zero.clone(), zero.clone(), zero.clone()]
        );
        assert_eq!(
            border(
                "a",
                ".a { border-top: 2px solid; border-style: hidden none; }"
            )[0],
            zero
        );
        assert_eq!(
            border("a", ".a { border-top: 2px; }")[0],
            zero,
            "style omitted = none"
        );
        assert_eq!(border("a", ".a { border: 2px solid; }")[0], Some(px(2.0)));
        assert_eq!(
            border("a", ".a { border-bottom: 3px dotted; }")[2],
            Some(px(3.0))
        );
    }

    #[test]
    fn global_keywords_resolve_in_the_cascade() {
        let page =
            r#"<!DOCTYPE html><html><body><div class="d"><p class="p">x</p></div></body></html>"#;
        let p_of = |css: &str| {
            find_tag(
                &style_page(
                    page,
                    &format!(".d {{ color: #ff0000; margin-left: 5px; }} {css}"),
                ),
                "p",
            )
            .unwrap()
            .clone()
        };
        let red = Value::Color(Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        });
        let blue = Value::Color(Color {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        });
        // inherit: the parent's computed value, inherited property or not.
        let p = p_of("p { color: blue; } .p { color: inherit; margin-left: inherit; }");
        assert_eq!(p.value("color"), Some(&red));
        assert_eq!(p.value("margin-left"), Some(&px(5.0)));
        // initial: no value (the consumer's initial), even for an inherited one.
        let p = p_of(".p { color: initial; margin-left: initial; }");
        assert_eq!(p.value("color"), None);
        assert_eq!(p.value("margin-left"), None);
        // unset / revert / revert-layer (≈ unset): inherited → parent, else initial.
        for kw in ["unset", "revert", "revert-layer"] {
            let p = p_of(&format!(
                "p {{ color: blue; margin-left: 9px; }} .p {{ color: {kw}; margin-left: {kw}; }}"
            ));
            assert_eq!(p.value("color"), Some(&red), "{kw}");
            assert_eq!(p.value("margin-left"), None, "{kw}");
        }
        let _ = blue;
    }

    #[test]
    fn all_unset_resets_earlier_declarations_and_ua_defaults() {
        let page = r#"<!DOCTYPE html><html><body><div class="d"><button class="b">x</button></div></body></html>"#;
        let styled = style_page(
            page,
            ".d { color: #ff0000; } button { margin-left: 7px; } .b { all: unset; padding-left: 2px; }",
        );
        let button = find_tag(&styled, "button").unwrap();
        assert_eq!(
            button.value("margin-left"),
            None,
            "earlier author value reset"
        );
        assert_eq!(
            button.value("padding-left"),
            Some(&px(2.0)),
            "later declaration applies"
        );
        assert_eq!(button.value("border-top"), None, "UA border reset");
        assert_eq!(
            button.value("color"),
            Some(&Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255
            })),
            "unset inherits"
        );
    }

    // ---- 1c: custom properties and var() ----

    fn p_with(css: &str) -> style::StyledNode {
        styled_p(r#"<p class="a b">x</p>"#, css)
    }

    fn red() -> Value {
        Value::Color(Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        })
    }

    #[test]
    fn rgb_of_var_resolves_after_cascade() {
        let p = p_with(":root { --x: 255,0,0; } p { color: rgb(var(--x)); }");
        assert_eq!(p.value("color"), Some(&red()));
        let p = p_with(":root { --x: 255, 0, 0; } p { color: rgba(var(--x), 1); }");
        assert_eq!(p.value("color"), Some(&red()));
    }

    #[test]
    fn shorthand_with_var_expands_after_substitution() {
        let p = p_with(":root { --x: 7px; } p { padding: 5px var(--x); }");
        let sides =
            ["top", "right", "bottom", "left"].map(|s| p.value(&format!("padding-{s}")).cloned());
        assert_eq!(
            sides,
            [Some(px(5.0)), Some(px(7.0)), Some(px(5.0)), Some(px(7.0))]
        );
        let p = p_with(":root { --c: #ff0000; } p { border: 2px solid var(--c); }");
        assert_eq!(p.value("border-left-color"), Some(&red()));
        assert_eq!(p.value("border-left"), Some(&px(2.0)));
    }

    #[test]
    fn invalid_after_substitution_falls_back_to_inherited() {
        // Invalid at computed-value time = `unset`: an inherited property
        // takes the parent's value, another its initial value.
        let p = p_with(
            "div { color: #ff0000; } :root { --x: foo; } p { color: var(--x); width: var(--x); }",
        );
        assert_eq!(p.value("color"), Some(&red()));
        assert_eq!(p.value("width"), None);
    }

    #[test]
    fn iacvt_does_not_fall_back_to_earlier_declaration() {
        let p = p_with(
            ".a { background-color: red; width: 3px; } .b { background-color: var(--undef); width: calc(var(--w) + 1px); }",
        );
        assert_eq!(p.value("background-color"), None);
        assert_eq!(
            p.value("width"),
            None,
            "calc() is step 5: invalid, not the earlier 3px"
        );
    }

    #[test]
    fn shorthand_var_and_longhand_cascade_in_order() {
        let p = p_with(":root { --x: 9px; } .a { padding-left: 3px; } .a.b { padding: var(--x); }");
        assert_eq!(p.value("padding-left"), Some(&px(9.0)));
        let p = p_with(":root { --x: 9px; } .a.b { padding-left: 3px; } .a { padding: var(--x); }");
        assert_eq!(p.value("padding-left"), Some(&px(3.0)));
        assert_eq!(p.value("padding-top"), Some(&px(9.0)));
    }

    #[test]
    fn custom_property_inherits_substituted_value() {
        // Custom properties inherit their computed (substituted) value:
        // `--a` is resolved on :root, a child's `--b` doesn't change it.
        let styled = style_page(
            r#"<!DOCTYPE html><html><body><div class="d">x</div></body></html>"#,
            ":root { --b: 1px; --a: var(--b); } div { --b: 9px; width: var(--a); height: var(--b); }",
        );
        let div = find_tag(&styled, "div").unwrap();
        assert_eq!(div.value("width"), Some(&px(1.0)));
        assert_eq!(div.value("height"), Some(&px(9.0)));
    }

    #[test]
    fn var_fallbacks_take_commas_and_nest() {
        let p = p_with("p { margin: var(--none, 1px 2px); color: var(--n1, var(--n2, #ff0000)); }");
        assert_eq!(p.value("margin-right"), Some(&px(2.0)));
        assert_eq!(p.value("color"), Some(&red()));
        // A defined-but-empty property substitutes nothing: `4px `.
        let p = p_with(":root { --e:; } p { width: 4px var(--e); }");
        assert_eq!(p.value("width"), Some(&px(4.0)));
    }

    #[test]
    fn defined_var_ignores_an_unresolvable_fallback() {
        // GitHub's `color: var(--fgColor-default,var(--color-fg-default))`:
        // the fallback is never needed when the name is defined.
        let p = p_with(":root { --fg: #ff0000; } p { color: var(--fg,var(--nope)); }");
        assert_eq!(p.value("color"), Some(&red()));
        let p = p_with(
            ":root { --fg: #ff0000; } p { color: rgb(255 0 0 / 1); color: var(--fg, var(--nope)); }",
        );
        assert_eq!(p.value("color"), Some(&red()));
    }

    #[test]
    fn custom_property_cycle_is_invalid_and_uses_the_fallback() {
        let p =
            p_with("p { --a: var(--b); --b: var(--a); width: var(--a, 3px); height: var(--b); }");
        assert_eq!(p.value("width"), Some(&px(3.0)));
        assert_eq!(p.value("height"), None);
    }

    #[test]
    fn exponential_var_expansion_is_bounded() {
        // Each level doubles the text: 2^40 tokens if nothing stopped it.
        let mut css = String::from(":root { --a0: x;");
        for i in 1..=40 {
            css.push_str(&format!(" --a{i}: var(--a{}) var(--a{});", i - 1, i - 1));
        }
        css.push_str(" } p { width: var(--a40); color: #ff0000; }");
        let started = std::time::Instant::now();
        let p = p_with(&css);
        assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
        assert_eq!(p.value("width"), None);
        assert_eq!(p.value("color"), Some(&red()));
    }

    #[test]
    fn unparsed_values_never_reach_the_styled_tree() {
        let p = p_with(":root { --x: 1px; } p { width: var(--x); margin: var(--x) var(--nope); }");
        assert!(
            p.specified_values
                .values()
                .all(|v| !matches!(v, Value::Unparsed { .. })),
            "{:?}",
            p.specified_values
        );
    }

    #[test]
    fn html_color_inherits_into_the_body() {
        let styled = style_page(PAGE, "html { color: #00ff00; }");
        assert_eq!(
            find_tag(&styled, "p").unwrap().value("color"),
            Some(&Value::Color(Color {
                r: 0,
                g: 255,
                b: 0,
                a: 255
            }))
        );
    }

    fn find_tag<'a>(node: &'a style::StyledNode, tag: &str) -> Option<&'a style::StyledNode> {
        if matches!(&node.node_type, crate::dom::NodeType::Element(el) if el.tag_name == tag) {
            return Some(node);
        }
        node.children.iter().find_map(|child| find_tag(child, tag))
    }

    // ---- step 5a: calc() and viewport units at style time ----

    /// The `<p>` inside `<div>` styled at a 1000×500 viewport.
    fn child_values(css: &str) -> (style::StyledNode, style::StyledNode) {
        let (document, root) = parse_html(r#"<div class="p"><p class="c">x</p></div>"#);
        let styled = style::style_tree_with_state(
            &document,
            root,
            &[parse_css(css)],
            style::InteractionState::default(),
            &crate::css::media::MediaContext::new(1000.0, 500.0),
        );
        let child = styled.children[0].clone();
        (styled, child)
    }

    #[test]
    fn font_size_calc_percent_uses_parent_font() {
        let (_, p) = child_values(".p { font-size: 20px } .c { font-size: calc(50% + 2px) }");
        assert_eq!(p.value("font-size"), Some(&Value::Length(12.0, Unit::Px)));
        let (_, p) = child_values(".p { font-size: 20px } .c { font-size: calc(1em + 1vw) }");
        assert_eq!(p.value("font-size"), Some(&Value::Length(30.0, Unit::Px)));
        let (_, p) = child_values(".p { font-size: 20px } .c { font-size: min(50%, 1rem) }");
        assert_eq!(p.value("font-size"), Some(&Value::Length(10.0, Unit::Px)));
    }

    #[test]
    fn line_height_calc_resolves_at_style_time() {
        let (_, p) = child_values(".c { font-size: 10px; line-height: calc(1em + 4px) }");
        assert_eq!(p.value("line-height"), Some(&Value::Length(14.0, Unit::Px)));
        // A percentage of the element's own font size.
        let (_, p) = child_values(".c { font-size: 10px; line-height: calc(150% + 2px) }");
        assert_eq!(p.value("line-height"), Some(&Value::Length(17.0, Unit::Px)));
    }

    #[test]
    fn line_height_unitless_calc_inherits_as_factor() {
        let (div, p) = child_values(
            ".p { font-size: 10px; line-height: calc(1 + .5) } .c { font-size: 20px }",
        );
        assert_eq!(div.value("line-height"), Some(&Value::Number(1.5)));
        assert_eq!(p.value("line-height"), Some(&Value::Number(1.5)));
    }

    #[test]
    fn viewport_units_and_calc_resolve_at_style_time() {
        let (div, p) = child_values(
            ".p { width: 50vw; height: calc(100vh - 100px); min-height: 10vmin; max-width: 10vmax }
             .c { font-size: 10px; width: calc(100% - 2em); margin-left: clamp(1px, 1vw, 5px) }",
        );
        assert_eq!(div.value("width"), Some(&Value::Length(500.0, Unit::Px)));
        assert_eq!(div.value("height"), Some(&Value::Length(400.0, Unit::Px)));
        assert_eq!(
            div.value("min-height"),
            Some(&Value::Length(50.0, Unit::Px))
        );
        assert_eq!(
            div.value("max-width"),
            Some(&Value::Length(100.0, Unit::Px))
        );
        // `%` waits for layout; the em is folded.
        match p.value("width") {
            Some(Value::Calc(calc)) => {
                assert!(calc.has_percent());
                assert_eq!(calc.eval(200.0), 180.0);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(p.value("margin-left"), Some(&Value::Length(5.0, Unit::Px)));
    }

    #[test]
    fn calc_of_var_times_minus_one() {
        let (_, p) = child_values(
            ".p { --a: 4px; --b: calc(var(--a) + 1rem) }
             .c { margin-top: calc(var(--a) * -1); padding-left: var(--b) }",
        );
        assert_eq!(p.value("margin-top"), Some(&Value::Length(-4.0, Unit::Px)));
        assert_eq!(
            p.value("padding-left"),
            Some(&Value::Length(20.0, Unit::Px))
        );
    }

    #[test]
    fn style_pass_reports_viewport_height_use() {
        let (document, root) = parse_html(r#"<div class="p"><p class="c">x</p></div>"#);
        let reads = |css: &str| {
            style::style_tree_reading_viewport(
                &document,
                root,
                &[parse_css(css)],
                style::InteractionState::default(),
                &crate::css::media::MediaContext::new(1000.0, 500.0),
            )
            .1
        };
        assert!(!reads(".c { width: 50vw; height: calc(100% - 1em) }"));
        assert!(reads(".c { height: 100vh }"));
        assert!(reads(".c { width: calc(10vmin + 1px) }"));
        assert!(reads(".c { margin-top: min(1px, 10vmax) }"));
    }

    #[test]
    fn a_track_list_on_every_element_is_shared_not_copied() {
        // `* { grid-template-columns: repeat(99999999, [a] 1px) }` is 1000
        // tracks and 1001 names on every node: copying it per node (and
        // per layout clone) took a fuzzer case from 25 to 450ms (step 5b).
        // px tracks need no rewrite, so every node shares the parsed list.
        let shared = |node: &style::StyledNode| match node.value("grid-template-columns") {
            Some(Value::TrackList(list)) => std::sync::Arc::clone(list),
            other => panic!("{other:?}"),
        };
        let (div, p) = child_values("* { grid-template-columns: repeat(99999999, [a] 1px) }");
        assert_eq!(shared(&p).tracks.len(), 1000);
        assert!(std::sync::Arc::ptr_eq(&shared(&div), &shared(&p)));
        // A list that does need units resolved gets its own copy.
        let (_, p) = child_values(".c { grid-template-columns: 1em 1fr }");
        assert_eq!(
            shared(&p).tracks[0],
            crate::css::TrackSize::Length(16.0, Unit::Px)
        );
    }

    #[test]
    fn style_report_names_the_vars_a_failed_substitution_missed() {
        let css = "p { color: var(--nope); width: var(--cyc); --cyc: var(--cyc); \
                   margin-left: var(--nope2, 1px); box-shadow: var(--s) } div { --s: none }";
        let report = report_for("<div><p>x</p></div>", css);
        let missing = |prop: &str| report.missing_vars.get(&sheet_key(css, prop)).cloned();
        assert_eq!(missing("color"), Some(["--nope".to_string()].into()));
        // A cycle reads as missing here too; the probe tells them apart by
        // whether the page declares the name.
        assert_eq!(missing("width"), Some(["--cyc".to_string()].into()));
        // Resolved through its fallback: no failure, nothing recorded.
        assert_eq!(missing("margin-left"), None);
        // `--s: none` is a real `box-shadow` value, not a rejected keyword.
        assert!(report.var_keyword_fallback.is_empty(), "{report:?}");
        assert!(
            !report.iacvt.contains(&sheet_key(css, "box-shadow")),
            "{report:?}"
        );
    }
}
