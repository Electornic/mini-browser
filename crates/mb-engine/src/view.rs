// Per-frame display-list builder plus the geometry helpers the UI loop needs
// to handle clicks, hovers, and scroll bounds. Pure functions — `BrowserState`
// calls in once per frame and gets back a `DocumentView` (paint commands +
// click rects + the layout root for hit testing). Chrome painting happens
// separately in `crate::chrome`; this module only knows about the page area.

use std::collections::HashMap;

use crate::{
    chrome::CHROME_HEIGHT,
    css, dom,
    dom::{NodeId, NodeType},
    input, layout, render, resource, style, url,
};
use mb_dom::profile;

#[derive(Debug, Clone)]
pub struct LinkTarget {
    pub href: String,
    pub rect: layout::Rect,
    pub underline: bool,
    /// The box's effective overflow clip (`layout::ClipTable`, step 3);
    /// the underline paints inside it. `None` = unclipped.
    pub clip: Option<layout::Rect>,
}

#[derive(Debug, Clone)]
pub struct DocumentView {
    // `commands` are what get painted, `links` are the separately tracked clickable regions.
    // `layout_root` is kept around so post-render hit-testing (e.g. computing :hover paths
    // from the mouse position) can walk the same boxes the painter saw.
    pub commands: Vec<render::DisplayCommand>,
    pub links: Vec<LinkTarget>,
    pub layout_root: layout::LayoutBox,
    /// Each box's effective overflow clip (compat round 3 step 3) — the one
    /// table paint, hit testing and the agent's click target read.
    pub clips: layout::ClipTable,
    /// The layout resolved the root's percentage `height`/`min-height`/
    /// `max-height` against the viewport height (step 1p) — so, unlike the
    /// rest of the view, it changes with a height-only resize.
    pub reads_viewport_height: bool,
    /// The style pass resolved a `vh`/`vmin`/`vmax` length (step 5a): the
    /// styled tree itself changes with a height-only resize.
    pub style_reads_viewport_height: bool,
    /// The scrollable height (step 1q), like Chrome's `scrollHeight` minus
    /// its viewport floor: the furthest of the root's margin-box bottom, any
    /// box's border-box bottom and any painted content — content can
    /// overflow a root sized to the viewport (`html, body {height: 100%}`).
    /// The canvas fill doesn't count. The one source for the scroll limit and
    /// the agent's `page_height`.
    pub document_height: f32,
}

// The deepest layout box under the mouse. `path` is the DOM-order index
// path used by the style pass for :hover/:focus rules; `node_id` is the
// back-reference into the arena used by Step 6 click dispatch. `node_id`
// is `None` only when the deepest hit is an anonymous block — none are
// produced today, so a None there reads as "no element under the cursor".
#[derive(Debug, Clone)]
pub struct HoverHit {
    pub path: Vec<usize>,
    pub node_id: Option<NodeId>,
}

pub fn build_document_view(
    parsed_document: &dom::Document,
    stylesheets: &[css::Stylesheet],
    viewport_width: usize,
    viewport_height: usize,
    current_url: Option<&url::Url>,
    images: &HashMap<String, resource::LoadedImage>,
    interaction: style::InteractionState<'_>,
) -> Result<DocumentView, String> {
    // The HTML/CSS parse steps used to live here and run every frame; they now
    // happen once at navigate time (see `BrowserState::install_document`) and
    // this function takes the cached trees, so the per-frame pipeline is just:
    // styled tree -> layout tree -> display commands + clickable metadata.
    //
    // `.last()` over the roots mirrors the original Vec<Node> behavior — when
    // the parser emits multiple top-level siblings (e.g. fragment-style HTML),
    // the visible page is the trailing one, which matches how a real browser
    // treats stray content before `<html>` as preamble.
    let root = parsed_document
        .roots()
        .last()
        .copied()
        .ok_or_else(|| "document did not produce a root node".to_string())?;
    // `@media` sees the page viewport (CSS px, chrome excluded).
    let media = css::media::MediaContext::new(viewport_width as f32, viewport_height as f32);
    let (styled, style_reads_viewport_height) = profile::time_block("view::style_tree", || {
        style::style_tree_reading_viewport(parsed_document, root, stylesheets, interaction, &media)
    });
    let layout = profile::time_block("view::layout_tree", || {
        layout::layout_tree_with_viewport(
            &styled,
            viewport_width as f32,
            Some(viewport_height as f32),
        )
    });
    let paint_ctx = render::PaintContext {
        base_url: current_url,
        images,
    };
    let clips = profile::time_block("view::compute_clips", || layout::compute_clips(&layout));
    let (mut commands, hidden_commands) = profile::time_block("view::build_display_list", || {
        render::build_display_list_with_hidden(&layout, &paint_ctx, &clips)
    });
    profile::time_block("view::collect_image_commands", || {
        collect_image_commands_into(&layout, current_url, images, &clips, &mut commands);
    });
    // Measured before the canvas fill goes in (it spans the viewport).
    let scroll_height = margin_box_bottom(&layout)
        .max(layout_overflow_bottom(&layout, &clips))
        .max(document_height(&commands))
        // `visibility: hidden` content still scrolls (step 1a).
        .max(document_height(&hidden_commands));
    if let Some(color) = canvas_background(&layout) {
        // Cover the viewport and the whole document, under everything.
        let height = (viewport_height as f32).max(scroll_height);
        let canvas = layout::Rect {
            x: 0.0,
            y: 0.0,
            width: viewport_width as f32,
            height,
        };
        commands.insert(0, render::DisplayCommand::SolidRect(color, canvas));
    }
    let links = profile::time_block("view::collect_link_targets", || {
        let mut links = Vec::new();
        collect_link_targets_into(
            &layout,
            None,
            false,
            render::Affine::IDENTITY,
            &clips,
            &mut links,
        );
        links
    });
    // Only a percentage on the root can reach the initial containing
    // block's height; anything below it chains through the root.
    let reads_viewport_height =
        ["height", "min-height", "max-height"]
            .iter()
            .any(|name| match styled.value(name) {
                Some(css::Value::Length(_, css::Unit::Percent)) => true,
                Some(css::Value::Calc(calc)) => calc.has_percent(),
                _ => false,
            });
    Ok(DocumentView {
        commands,
        links,
        layout_root: layout,
        clips,
        reads_viewport_height,
        style_reads_viewport_height,
        document_height: scroll_height,
    })
}

/// The canvas colour (CSS Backgrounds 3 §2.11.2): the root `<html>`'s
/// `background-color`, or — when that is transparent — the `<body>`'s.
/// Only a colour propagates; a root background *image* still paints in its
/// own box only (not modelled). The box itself keeps painting its colour on
/// top — same colour, so only a translucent one shows twice.
fn canvas_background(layout: &layout::LayoutBox) -> Option<css::Color> {
    let opaque_color = |node: &style::StyledNode| match node.value("background-color") {
        Some(css::Value::Color(color)) if color.a > 0 => Some(*color),
        _ => None,
    };
    let html = layout.styled_node()?;
    if !matches!(&html.node_type, NodeType::Element(e) if e.tag_name.eq_ignore_ascii_case("html")) {
        return None;
    }
    opaque_color(html).or_else(|| {
        let body = html.children.iter().find(
            |child| matches!(&child.node_type, NodeType::Element(e) if e.tag_name.eq_ignore_ascii_case("body")),
        )?;
        opaque_color(body)
    })
}

/// The lowest border-box bottom in the layout tree — Chrome's scrollable
/// overflow counts boxes, not just what they paint (an empty 3,000px box
/// still scrolls). A box counts only as far as its overflow clip lets it
/// (compat round 3 step 3 — content cut by `overflow: hidden` doesn't
/// scroll), and not at all when the clip hides it. Iterative: layout trees
/// are as deep as the DOM.
fn layout_overflow_bottom(root: &layout::LayoutBox, clips: &layout::ClipTable) -> f32 {
    let mut bottom = 0.0_f32;
    let mut stack = vec![root];
    while let Some(layout_box) = stack.pop() {
        let d = &layout_box.dimensions;
        let border = layout::Rect {
            x: d.content.x - d.padding.left - d.border.left,
            y: d.content.y - d.padding.top - d.border.top,
            width: d.content.width
                + d.padding.left
                + d.padding.right
                + d.border.left
                + d.border.right,
            height: d.content.height
                + d.padding.top
                + d.padding.bottom
                + d.border.top
                + d.border.bottom,
        };
        let visible = match node_id_for_layout_box(layout_box) {
            Some(id) => clips.visible_rect(id, border),
            None => Some(border),
        };
        if let Some(visible) = visible {
            bottom = bottom.max(visible.y + visible.height);
        }
        stack.extend(layout_box.children.iter());
    }
    bottom
}

fn margin_box_bottom(layout_box: &layout::LayoutBox) -> f32 {
    let d = &layout_box.dimensions;
    d.content.y + d.content.height + d.padding.bottom + d.border.bottom + d.margin.bottom
}

pub fn document_height(commands: &[render::DisplayCommand]) -> f32 {
    commands.iter().fold(0.0, |max_bottom, command| {
        let bottom = command_bottom(command);
        max_bottom.max(bottom)
    })
}

fn command_bottom(command: &render::DisplayCommand) -> f32 {
    match command {
        render::DisplayCommand::SolidRect(_, rect) => rect.y + rect.height,
        render::DisplayCommand::RoundedRect(_, rect, _) => rect.y + rect.height,
        render::DisplayCommand::RoundedBorder(border) => border.rect.y + border.rect.height,
        render::DisplayCommand::Text(text) => text.y + text.font_size,
        render::DisplayCommand::Image(image) => image.y + image.height,
        render::DisplayCommand::Gradient(gradient) => gradient.rect.y + gradient.rect.height,
        render::DisplayCommand::BoxShadow(shadow) => shadow.rect.y + shadow.rect.height,
        render::DisplayCommand::ClipGroup(clip, inner) => inner
            .iter()
            .map(command_bottom)
            .fold(0.0_f32, f32::max)
            .min(clip.y + clip.height),
        render::DisplayCommand::TransformGroup(transform, inner) => {
            // Logical bottom is the max-y of inner commands; map every
            // inner command's logical bbox through the matrix and take the
            // worst y of the four projected corners. Anything bigger is a
            // false positive here, but better that than under-reporting and
            // clipping a rotated element off the bottom of the document.
            inner
                .iter()
                .map(|cmd| projected_command_bottom(cmd, *transform))
                .fold(0.0_f32, f32::max)
        }
    }
}

fn projected_command_bottom(command: &render::DisplayCommand, transform: render::Affine) -> f32 {
    let bounds = match command {
        render::DisplayCommand::SolidRect(_, rect) => *rect,
        render::DisplayCommand::RoundedRect(_, rect, _) => *rect,
        render::DisplayCommand::RoundedBorder(border) => border.rect,
        render::DisplayCommand::Text(text) => layout::Rect {
            x: text.x,
            y: text.y,
            // Bitmap-rasterised text doesn't know its own width here; for
            // overflow purposes the font_size box is a safe upper bound.
            width: text.font_size,
            height: text.font_size,
        },
        render::DisplayCommand::Image(image) => layout::Rect {
            x: image.x,
            y: image.y,
            width: image.width,
            height: image.height,
        },
        render::DisplayCommand::Gradient(gradient) => gradient.rect,
        render::DisplayCommand::BoxShadow(shadow) => shadow.rect,
        // Inner groups should never appear in practice; treat as 0.
        render::DisplayCommand::TransformGroup(_, _) | render::DisplayCommand::ClipGroup(_, _) => {
            return 0.0;
        }
    };
    let corners = [
        transform.apply_point(bounds.x, bounds.y),
        transform.apply_point(bounds.x + bounds.width, bounds.y),
        transform.apply_point(bounds.x + bounds.width, bounds.y + bounds.height),
        transform.apply_point(bounds.x, bounds.y + bounds.height),
    ];
    corners
        .iter()
        .map(|(_, y)| *y)
        .fold(f32::NEG_INFINITY, f32::max)
}

pub fn collect_link_targets(
    layout_box: &layout::LayoutBox,
    inherited_href: Option<&str>,
    inherited_no_underline: bool,
    inherited_transform: render::Affine,
) -> Vec<LinkTarget> {
    let mut targets = Vec::new();
    collect_link_targets_into(
        layout_box,
        inherited_href,
        inherited_no_underline,
        inherited_transform,
        &layout::ClipTable::default(),
        &mut targets,
    );
    targets
}

fn collect_link_targets_into(
    layout_box: &layout::LayoutBox,
    inherited_href: Option<&str>,
    inherited_no_underline: bool,
    inherited_transform: render::Affine,
    clips: &layout::ClipTable,
    targets: &mut Vec<LinkTarget>,
) {
    let own_href = href_for_layout_box(layout_box);
    let current_href = own_href.or(inherited_href);
    // text-decoration: none on any ancestor (typically the <a> itself) suppresses
    // underlines for everything below it.
    let no_underline = inherited_no_underline || has_text_decoration_none(layout_box);
    // Compose this box's own `transform` onto the inherited matrix the same
    // way the paint pass does. The link rect is stored in screen space so
    // click hit-testing and underline drawing can stay axis-aligned for the
    // translate-only support shipping in this commit.
    let effective_transform = inherited_transform.compose(render::transform_for(layout_box));

    // Link targets are collected separately from display commands because clicking needs rectangles,
    // not just painted pixels.
    // A hidden or inert box is no link target (step 1a — it doesn't
    // underline, hover or navigate); a `visible` child of a hidden link is.
    if let Some(href) = current_href
        .filter(|_| should_collect_link_target(layout_box, own_href) && hit_testable(layout_box))
    {
        let content = layout_box.dimensions.content;
        let (x, y) = effective_transform.apply_point(content.x, content.y);
        targets.push(LinkTarget {
            href: href.to_string(),
            rect: layout::Rect {
                x,
                y,
                width: content.width,
                height: content.height,
            },
            underline: own_href.is_none() && !no_underline,
            clip: node_id_for_layout_box(layout_box).and_then(|id| clips.clip_of(id)),
        });
    }

    for child in &layout_box.children {
        collect_link_targets_into(
            child,
            current_href,
            no_underline,
            effective_transform,
            clips,
            targets,
        );
    }
}

fn has_text_decoration_none(layout_box: &layout::LayoutBox) -> bool {
    match &layout_box.box_type {
        layout::BoxType::BlockNode(node)
        | layout::BoxType::FlexNode(node)
        | layout::BoxType::GridNode(node)
        | layout::BoxType::TableNode(node) => matches!(
            node.value("text-decoration"),
            Some(css::Value::Keyword(keyword)) if keyword == "none"
        ),
        layout::BoxType::AnonymousBlock => false,
    }
}

pub fn collect_image_commands(
    layout_box: &layout::LayoutBox,
    base_url: Option<&url::Url>,
    images: &HashMap<String, resource::LoadedImage>,
) -> Vec<render::DisplayCommand> {
    let mut commands = Vec::new();
    collect_image_commands_into(
        layout_box,
        base_url,
        images,
        &layout::ClipTable::default(),
        &mut commands,
    );
    commands
}

/// `<img>` commands under each image's overflow clip (a `ClipGroup` of one;
/// nothing when the clip hides it).
fn collect_image_commands_into(
    layout_box: &layout::LayoutBox,
    base_url: Option<&url::Url>,
    images: &HashMap<String, resource::LoadedImage>,
    clips: &layout::ClipTable,
    commands: &mut Vec<render::DisplayCommand>,
) {
    if let Some(command) = image_command_for_layout_box(layout_box, base_url, images)
        .filter(|_| !render::is_visibility_hidden(layout_box))
    {
        match node_id_for_layout_box(layout_box).and_then(|id| clips.clip_of(id)) {
            None => commands.push(command),
            Some(clip) if clip.width > 0.0 && clip.height > 0.0 => {
                commands.push(render::DisplayCommand::ClipGroup(clip, vec![command]))
            }
            Some(_) => {}
        }
    }

    for child in &layout_box.children {
        collect_image_commands_into(child, base_url, images, clips, commands);
    }
}

fn should_collect_link_target(layout_box: &layout::LayoutBox, own_href: Option<&str>) -> bool {
    if own_href.is_some() {
        return true;
    }

    matches!(
        &layout_box.box_type,
        layout::BoxType::BlockNode(styled_node)
            | layout::BoxType::FlexNode(styled_node)
            | layout::BoxType::GridNode(styled_node)
            | layout::BoxType::TableNode(styled_node)
            if matches!(styled_node.node_type, NodeType::Text(_))
    )
}

fn href_for_layout_box(layout_box: &layout::LayoutBox) -> Option<&str> {
    match &layout_box.box_type {
        layout::BoxType::BlockNode(styled_node)
        | layout::BoxType::FlexNode(styled_node)
        | layout::BoxType::GridNode(styled_node)
        | layout::BoxType::TableNode(styled_node) => match &styled_node.node_type {
            NodeType::Element(element) => element.attributes.get("href").map(String::as_str),
            NodeType::Text(_) => None,
        },
        layout::BoxType::AnonymousBlock => None,
    }
}

fn src_for_layout_box(layout_box: &layout::LayoutBox) -> Option<&str> {
    match &layout_box.box_type {
        layout::BoxType::BlockNode(styled_node)
        | layout::BoxType::FlexNode(styled_node)
        | layout::BoxType::GridNode(styled_node)
        | layout::BoxType::TableNode(styled_node) => match &styled_node.node_type {
            NodeType::Element(element) if element.tag_name == "img" => {
                element.attributes.get("src").map(String::as_str)
            }
            _ => None,
        },
        layout::BoxType::AnonymousBlock => None,
    }
}

fn image_command_for_layout_box(
    layout_box: &layout::LayoutBox,
    base_url: Option<&url::Url>,
    images: &HashMap<String, resource::LoadedImage>,
) -> Option<render::DisplayCommand> {
    // Layout decides *where* an image box goes; the image cache supplies *what* pixels fill it.
    let src = src_for_layout_box(layout_box)?;
    let image_key = if src.contains("://") {
        src.to_string()
    } else {
        base_url?.resolve(src).ok()?.to_string()
    };
    let image = images.get(&image_key)?;

    Some(render::DisplayCommand::Image(render::ImageCommand {
        x: layout_box.dimensions.content.x,
        y: layout_box.dimensions.content.y,
        width: layout_box.dimensions.content.width,
        height: layout_box.dimensions.content.height,
        source_width: image.width,
        source_height: image.height,
        pixels: std::rc::Rc::new(image.pixels.clone()),
        source_x: 0.0,
        source_y: 0.0,
        tile: None,
    }))
}

pub fn point_in_rect(x: f32, y: f32, rect: layout::Rect) -> bool {
    x >= rect.x && x <= rect.x + rect.width && y >= rect.y && y <= rect.y + rect.height
}

// Thin convenience wrapper that drops the NodeId. Used by hover tests
// that compare against the path slice; the production caller
// (`BrowserState::display_list`) reaches for `compute_hovered_hit`
// directly so it can also feed the deepest hit's NodeId into click
// dispatch.
pub fn compute_hovered_dom_path(
    input: &input::WindowInput,
    layout_root: &layout::LayoutBox,
    scroll_offset: f32,
) -> Option<Vec<usize>> {
    compute_hovered_hit(
        input,
        layout_root,
        &layout::ClipTable::default(),
        scroll_offset,
    )
    .map(|hit| hit.path)
}

/// The topmost box under the pointer, the way paint stacks boxes (compat
/// round 4 step 1b-z): the painter's own order (`render::paint_order` —
/// z-index and positioned layers), last match wins. A box whose overflow
/// clip (`clips`, the frame's `DocumentView::clips`) excludes the point is
/// not a hit — but its descendants are still tested (an absolute child can
/// sit under a different clip). Boxes the hit test passes through:
/// `visibility: hidden`, `pointer-events: none` (own record only — a
/// `visible`/`auto` child still takes clicks) and inert subtrees. Human
/// clicks, link navigation (1b-link) and the agent's synthetic clicks all
/// come through here.
pub fn compute_hovered_hit(
    input: &input::WindowInput,
    layout_root: &layout::LayoutBox,
    clips: &layout::ClipTable,
    scroll_offset: f32,
) -> Option<HoverHit> {
    // Hover is only meaningful when the pointer is over the page area (i.e. below the
    // chrome). Anywhere else — chrome, off-window — leaves the styled tree in its
    // "nothing hovered" state.
    let (mouse_x, mouse_y) = input.mouse_position?;
    if mouse_y < CHROME_HEIGHT {
        return None;
    }
    let doc_y = mouse_y - CHROME_HEIGHT + scroll_offset;
    let hit = topmost_box_at(layout_root, mouse_x, doc_y, clips)?;
    // The layout child-index path to the box (the fallback path for a hit
    // without a NodeId — callers rebuild DOM paths from the NodeId).
    let mut path = Vec::new();
    layout_path_to(layout_root, hit, &mut path);
    Some(HoverHit {
        path,
        node_id: node_id_for_layout_box(hit),
    })
}

/// The node [`compute_hovered_hit`] picks. Kept as its own entry for the
/// reach check (compat round 4 P2), which before step 1b-z compared it with
/// the old document-order hit to tell a z-order miss apart; the two are the
/// same function now.
pub fn hit_node_in_paint_order(
    input: &input::WindowInput,
    layout_root: &layout::LayoutBox,
    clips: &layout::ClipTable,
    scroll_offset: f32,
) -> Option<NodeId> {
    compute_hovered_hit(input, layout_root, clips, scroll_offset)?.node_id
}

/// The last box in paint order whose padding box (under its transform)
/// holds page point (`x`, `doc_y`) inside its clip — the one on top.
fn topmost_box_at<'a>(
    layout_root: &'a layout::LayoutBox,
    x: f32,
    doc_y: f32,
    clips: &layout::ClipTable,
) -> Option<&'a layout::LayoutBox> {
    let mut best = None;
    for (layout_box, transform) in render::paint_order(layout_root) {
        if !hit_testable(layout_box) {
            continue;
        }
        let (logical_x, logical_y) = transform.inverse().apply_point(x, doc_y);
        let clip = node_id_for_layout_box(layout_box).and_then(|id| clips.clip_of(id));
        if point_in_rect(logical_x, logical_y, padding_box(layout_box))
            && point_in_clip(x, doc_y, clip)
        {
            best = Some(layout_box);
        }
    }
    best
}

/// Child indices from `layout_box` down to `target` (by identity).
fn layout_path_to(
    layout_box: &layout::LayoutBox,
    target: &layout::LayoutBox,
    path: &mut Vec<usize>,
) -> bool {
    if std::ptr::eq(layout_box, target) {
        return true;
    }
    for (index, child) in layout_box.children.iter().enumerate() {
        path.push(index);
        if layout_path_to(child, target, path) {
            return true;
        }
        path.pop();
    }
    false
}

/// Whether page point (`x`, `y`) is inside `clip` (`None` = unclipped; a
/// zero-area clip contains nothing).
pub fn point_in_clip(x: f32, y: f32, clip: Option<layout::Rect>) -> bool {
    clip.is_none_or(|c| c.width > 0.0 && c.height > 0.0 && point_in_rect(x, y, c))
}

/// Whether the hit test may stop at this box (steps 1a/1b): not
/// `visibility: hidden` and not `pointer-events: none` (its descendants are
/// still tested — a `visible`/`auto` child takes clicks) and not inert (the
/// whole subtree is, by inheritance).
fn hit_testable(layout_box: &layout::LayoutBox) -> bool {
    match &layout_box.box_type {
        layout::BoxType::BlockNode(node)
        | layout::BoxType::FlexNode(node)
        | layout::BoxType::GridNode(node)
        | layout::BoxType::TableNode(node) => {
            !node.visibility_hidden() && !node.pointer_events_none() && !node.inert()
        }
        layout::BoxType::AnonymousBlock => true,
    }
}

fn node_id_for_layout_box(layout_box: &layout::LayoutBox) -> Option<NodeId> {
    match &layout_box.box_type {
        layout::BoxType::BlockNode(node)
        | layout::BoxType::FlexNode(node)
        | layout::BoxType::GridNode(node)
        | layout::BoxType::TableNode(node) => Some(node.node_id),
        layout::BoxType::AnonymousBlock => None,
    }
}

fn padding_box(layout_box: &layout::LayoutBox) -> layout::Rect {
    let dims = &layout_box.dimensions;
    let content = dims.content;
    let pad = dims.padding;
    layout::Rect {
        x: content.x - pad.left,
        y: content.y - pad.top,
        width: content.width + pad.left + pad.right,
        height: content.height + pad.top + pad.bottom,
    }
}

/// Emits the blinking 1px caret that sits at the end of a focused
/// `<input>` or `<textarea>`'s value text. The caret turns on for 30
/// frames, off for 30, repeating — same `(frame_index / 30).is_multiple_of(2)`
/// cadence the chrome address bar caret uses, so both blinks stay in
/// phase. Returns an empty Vec when no field is focused or when the
/// blink is in its "off" half; callers extend their command list
/// unconditionally and the empty case becomes a no-op paint.
///
/// `focused_node_id` is what links the styled-tree focus state to a
/// specific layout box — `BrowserState::focused_dom_path` resolves to
/// this NodeId on each frame, so the caret follows DOM mutations
/// (renaming, swapping the focused element) without going stale.
///
/// For a `<textarea>` the caret rides the *last* `\n`-delimited line:
/// y is offset by `font_size * (lines - 1)`, x is the measured width
/// of the trailing line. That mirrors how the value-text paint code
/// stacks lines top-to-bottom from the content origin.
pub fn caret_commands_for_focused_input(
    layout_root: &layout::LayoutBox,
    focused_node_id: Option<NodeId>,
    frame_index: usize,
) -> Vec<render::DisplayCommand> {
    let Some(focused_id) = focused_node_id else {
        return Vec::new();
    };
    if !(frame_index / 30).is_multiple_of(2) {
        return Vec::new();
    }
    let Some(input_box) = find_focused_text_field_box(layout_root, focused_id) else {
        return Vec::new();
    };

    // Pull the cascaded font-size and current value off the styled node so
    // the caret position uses the SAME font size that paint_self used to
    // draw the value text — otherwise wide-glyph fonts would put the caret
    // in the wrong column.
    let (value, font_size, is_textarea) = match &input_box.box_type {
        layout::BoxType::BlockNode(node)
        | layout::BoxType::FlexNode(node)
        | layout::BoxType::GridNode(node)
        | layout::BoxType::TableNode(node) => {
            let (value, is_textarea) = match &node.node_type {
                // Same text the field paints (bullets for passwords), so
                // the caret lands after what's visible, not after the
                // hidden value's real glyph widths.
                NodeType::Element(elem) => (
                    render::field_display_value(elem).unwrap_or_default(),
                    elem.tag_name == "textarea",
                ),
                NodeType::Text(_) => return Vec::new(),
            };
            let size = match node.value("font-size") {
                Some(css::Value::Length(v, css::Unit::Px)) => *v,
                _ => 16.0,
            };
            (value, size, is_textarea)
        }
        layout::BoxType::AnonymousBlock => return Vec::new(),
    };

    // Single-line `<input>` keeps the original layout (caret at end of
    // value, vertically aligned with the glyph row). `<textarea>` rides
    // the last `\n`-delimited line so a freshly-typed Enter parks the
    // caret at the start of an empty next row.
    let (caret_x_offset, caret_y_offset) = if is_textarea {
        let mut row_count = 0usize;
        let mut last_line = "";
        for line in value.split('\n') {
            last_line = line;
            row_count += 1;
        }
        let row = row_count.saturating_sub(1);
        let x_off = render::measure_text_width(last_line, font_size);
        (x_off, font_size * row as f32)
    } else {
        (render::measure_text_width(&value, font_size), 0.0)
    };

    let content = input_box.dimensions.content;
    vec![render::DisplayCommand::SolidRect(
        css::Color::BLACK,
        layout::Rect {
            x: content.x + caret_x_offset,
            // The chrome caret bleeds 1px above and below the glyph row to
            // give the bar a slightly thicker visual footprint; mirror
            // that here so the page input caret reads the same.
            y: content.y + caret_y_offset - 1.0,
            width: 1.0,
            height: font_size + 2.0,
        },
    )]
}

// Depth-first search for the layout box that backs the focused
// `<input>` or `<textarea>`. We can't pre-compute this from
// `focused_dom_path` alone because the layout box's `node_id` is the
// canonical identity — DOM mutations between the focus event and the
// next paint can move the same NodeId to a different position in the
// tree (insertBefore, etc.) and the caret has to follow.
fn find_focused_text_field_box(
    box_node: &layout::LayoutBox,
    focused_id: NodeId,
) -> Option<&layout::LayoutBox> {
    let is_focused_field = match &box_node.box_type {
        layout::BoxType::BlockNode(node)
        | layout::BoxType::FlexNode(node)
        | layout::BoxType::GridNode(node)
        | layout::BoxType::TableNode(node) => {
            node.node_id == focused_id
                && matches!(
                    &node.node_type,
                    NodeType::Element(elem) if elem.tag_name == "input" || elem.tag_name == "textarea"
                )
        }
        layout::BoxType::AnonymousBlock => false,
    };
    if is_focused_field {
        return Some(box_node);
    }
    for child in &box_node.children {
        if let Some(found) = find_focused_text_field_box(child, focused_id) {
            return Some(found);
        }
    }
    None
}

pub fn link_decoration_commands(
    links: &[LinkTarget],
    hovered_href: Option<&str>,
) -> Vec<render::DisplayCommand> {
    // Link underlines are drawn as separate commands so hover state can change them cheaply.
    links
        .iter()
        .filter(|link| link.underline)
        // A clip that hides the link hides its underline too.
        .filter(|link| link.clip.is_none_or(|c| c.width > 0.0 && c.height > 0.0))
        .map(|link| {
            let color = if hovered_href == Some(link.href.as_str()) {
                css::Color {
                    r: 180,
                    g: 60,
                    b: 140,
                    a: 255,
                }
            } else {
                css::Color {
                    r: 0,
                    g: 102,
                    b: 204,
                    a: 255,
                }
            };

            let underline = render::DisplayCommand::SolidRect(
                color,
                layout::Rect {
                    x: link.rect.x,
                    y: link.rect.y + link.rect.height.max(1.0) - 1.0,
                    width: link.rect.width.max(1.0),
                    height: 1.0,
                },
            );
            match link.clip {
                Some(clip) => render::DisplayCommand::ClipGroup(clip, vec![underline]),
                None => underline,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{html, render::DisplayCommand};

    // Builds a layout tree from `html_source` and returns it together with
    // the NodeId of the first <input> in document order. Lets the caret
    // tests below stay terse: each test only cares about "the input" and
    // the helper hides the parse/style/layout boilerplate.
    fn setup(html_source: &str) -> (layout::LayoutBox, NodeId) {
        let document = html::parse(html_source).unwrap();
        let root = document.roots()[0];
        let styled = style::style_tree(&document, root, &[]);
        let layout_root = layout::layout_tree(&styled, 400.0);
        let input_id = find_input_node_id(&document, root)
            .expect("test fixtures must contain at least one <input>");
        (layout_root, input_id)
    }

    fn find_input_node_id(document: &dom::Document, current: NodeId) -> Option<NodeId> {
        let node = document.get(current)?;
        if let NodeType::Element(elem) = &node.node_type
            && elem.tag_name == "input"
        {
            return Some(current);
        }
        for child in &node.children {
            if let Some(found) = find_input_node_id(document, *child) {
                return Some(found);
            }
        }
        None
    }

    fn page_view(html_source: &str, css_source: &str) -> DocumentView {
        let document = html::parse(html_source).unwrap();
        let sheet = css::parse(css_source).unwrap();
        build_document_view(
            &document,
            std::slice::from_ref(&sheet),
            400,
            300,
            None,
            &HashMap::new(),
            style::InteractionState::default(),
        )
        .unwrap()
    }

    fn canvas_rect(view: &DocumentView) -> Option<(css::Color, layout::Rect)> {
        match view.commands.first() {
            Some(DisplayCommand::SolidRect(color, rect)) if rect.x == 0.0 && rect.y == 0.0 => {
                Some((*color, *rect))
            }
            _ => None,
        }
    }

    const SHORT_PAGE: &str = "<html><body><p>x</p></body></html>";

    /// The node under page point (`x`, `y`) with and without the clip table.
    fn hit_with_clips(html_source: &str, css_source: &str, x: f32, y: f32) -> [Option<String>; 2] {
        let document = html::parse(html_source).unwrap();
        let root = document.roots()[0];
        let sheet = css::parse(css_source).unwrap();
        let styled = style::style_tree(&document, root, &[sheet]);
        let layout_root = layout::layout_tree(&styled, 400.0);
        let clips = layout::compute_clips(&layout_root);
        let input = input::WindowInput {
            mouse_position: Some((x, y + CHROME_HEIGHT)),
            ..input::WindowInput::default()
        };
        let id_of = |hit: Option<HoverHit>| {
            let id = hit?.node_id?;
            match &document.get(id)?.node_type {
                NodeType::Element(e) => e.attributes.get("id").cloned(),
                NodeType::Text(_) => {
                    let parent = document.get(id)?.parent?;
                    document.element_data(parent)?.attributes.get("id").cloned()
                }
            }
        };
        [
            id_of(compute_hovered_hit(
                &input,
                &layout_root,
                &layout::ClipTable::default(),
                0.0,
            )),
            id_of(compute_hovered_hit(&input, &layout_root, &clips, 0.0)),
        ]
    }

    #[test]
    fn paint_order_hit_sees_a_higher_z_index_drawn_over_a_later_box() {
        // #top comes first in the document but paints above #later
        // (z-index): the DFS hit test picks #later, the paint-order one
        // #top — the z_order case of compat round 4 P2.
        let css = "#top { position: absolute; left: 0; top: 0; width: 100px; height: 50px; z-index: 5; } \
                   #later { position: relative; width: 100px; height: 50px; }";
        let html = "<div id=page><div id=top></div><div id=later></div></div>";
        let document = html::parse(html).unwrap();
        let root = document.roots()[0];
        let sheet = css::parse(css).unwrap();
        let styled = style::style_tree(&document, root, &[sheet]);
        let layout_root = layout::layout_tree(&styled, 400.0);
        let clips = layout::compute_clips(&layout_root);
        let input = input::WindowInput {
            mouse_position: Some((10.0, 10.0 + CHROME_HEIGHT)),
            ..input::WindowInput::default()
        };
        let attr_id =
            |id: Option<NodeId>| document.element_data(id?)?.attributes.get("id").cloned();
        // Step 1b-z: the hit test itself walks in paint order now.
        let hit = compute_hovered_hit(&input, &layout_root, &clips, 0.0).and_then(|h| h.node_id);
        assert_eq!(attr_id(hit).as_deref(), Some("top"));
        let painted = hit_node_in_paint_order(&input, &layout_root, &clips, 0.0);
        assert_eq!(attr_id(painted).as_deref(), Some("top"));
    }

    fn texts(commands: &[DisplayCommand]) -> Vec<String> {
        let mut out = Vec::new();
        render::walk_commands(commands, &mut |c| {
            if let DisplayCommand::Text(t) = c {
                out.push(t.text.trim().to_string());
            }
        });
        out
    }

    fn image_count(commands: &[DisplayCommand]) -> usize {
        let mut n = 0;
        render::walk_commands(commands, &mut |c| {
            if matches!(c, DisplayCommand::Image(_)) {
                n += 1;
            }
        });
        n
    }

    /// A view with one 2×2 image at `http://t/a.png` (and its base URL).
    fn view_with_image(html_source: &str, css_source: &str) -> DocumentView {
        let document = html::parse(html_source).unwrap();
        let sheet = css::parse(css_source).unwrap();
        let base = url::Url::parse("http://t/").unwrap();
        let image = resource::LoadedImage {
            url: url::Url::parse("http://t/a.png").unwrap(),
            width: 2,
            height: 2,
            pixels: vec![0xff00_00ff; 4],
        };
        let images = HashMap::from([("http://t/a.png".to_string(), image)]);
        build_document_view(
            &document,
            std::slice::from_ref(&sheet),
            400,
            300,
            Some(&base),
            &images,
            style::InteractionState::default(),
        )
        .unwrap()
    }

    #[test]
    fn visibility_hidden_is_not_painted_hit_or_linked() {
        // Compat round 4 step 1a (D5): the hidden menu's background, text,
        // image and link are gone; its `visible` child still paints, is
        // hit and links. The box keeps its space.
        let html = "<html><body><div id=menu><a id=a href=/x>Hidden <img src=a.png></a>\
                    <a id=b href=/y><span id=on>Shown</span></a></div></body></html>";
        let css = "body { margin: 0; } #menu { visibility: hidden; background-color: #ff0000; height: 40px; } \
                   #on { visibility: visible; } img { width: 10px; height: 10px; }";
        let view = view_with_image(html, css);
        assert_eq!(texts(&view.commands), vec!["Shown".to_string()]);
        assert_eq!(image_count(&view.commands), 0, "the hidden link's image");
        let red = css::Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let mut red_fill = false;
        render::walk_commands(&view.commands, &mut |c| {
            red_fill |= matches!(c, DisplayCommand::SolidRect(color, _) if *color == red);
        });
        assert!(!red_fill, "the hidden box's background");
        let hrefs: Vec<&str> = view.links.iter().map(|l| l.href.as_str()).collect();
        assert_eq!(
            hrefs,
            vec!["/y"],
            "only the visible child links (and underlines)"
        );
        // Without `visibility` the same page paints all of it.
        let shown = view_with_image(
            html,
            "body { margin: 0; } img { width: 10px; height: 10px; }",
        );
        assert_eq!(image_count(&shown.commands), 1);
        assert!(shown.links.iter().any(|l| l.href == "/x"));
    }

    #[test]
    fn hit_test_skips_a_hidden_box_but_not_its_visible_child() {
        let css = "#menu { visibility: hidden; width: 200px; height: 40px; } \
                   #on { visibility: visible; display: block; width: 50px; height: 20px; }";
        let html = "<div id=page><div id=menu><div id=on></div></div></div>";
        // Over the hidden box only: the page under it.
        assert_eq!(
            hit_with_clips(html, css, 150.0, 30.0)[1].as_deref(),
            Some("page")
        );
        // Over its visible child: the child.
        assert_eq!(
            hit_with_clips(html, css, 10.0, 10.0)[1].as_deref(),
            Some("on")
        );
    }

    #[test]
    fn inert_subtree_is_painted_but_not_hit_or_linked() {
        // N4: Chrome paints `inert` content; hit testing passes through it
        // (HTML §6.3, like `pointer-events: none`), so its link can't be
        // followed or hovered.
        let html = "<html><body><div id=page><div inert id=panel><a href=/x>Draft</a></div></div></body></html>";
        let css = "body { margin: 0; } #panel { width: 200px; height: 40px; }";
        let view = page_view(html, css);
        assert_eq!(texts(&view.commands), vec!["Draft".to_string()]);
        assert!(view.links.is_empty());
        assert_eq!(
            hit_with_clips(html, css, 10.0, 10.0)[1].as_deref(),
            Some("page")
        );
    }

    #[test]
    fn visibility_hidden_keeps_layout_space_and_document_height() {
        // Chrome counts hidden boxes in the scroll overflow: the page is as
        // tall as with the box shown — even where only what it paints
        // reaches that far (our height counts painted commands too: here a
        // shadow 500px below the box).
        let html = "<html><body><div id=menu>menu</div><p id=after>after</p></body></html>";
        let base = "body { margin: 0; } #menu { height: 10px; box-shadow: 0 500px 0 0 #000000; } p { margin: 0; }";
        let shown = page_view(html, base);
        let hidden = page_view(html, &format!("{base} #menu {{ visibility: hidden; }}"));
        assert!(shown.document_height > 500.0, "{}", shown.document_height);
        assert_eq!(hidden.document_height, shown.document_height);
        assert_eq!(texts(&hidden.commands), vec!["after".to_string()]);
        // `after` sits where it did.
        let after_y = |view: &DocumentView| {
            let mut y = None;
            render::walk_commands(&view.commands, &mut |c| {
                if let DisplayCommand::Text(t) = c
                    && t.text.trim() == "after"
                {
                    y = Some(t.y);
                }
            });
            y
        };
        assert_eq!(after_y(&hidden), after_y(&shown));
    }

    #[test]
    fn pointer_events_none_box_is_skipped_but_an_auto_child_is_hit() {
        // Step 1b: a `pointer-events: none` decoration over a button lets
        // the click through; its `auto` child still takes clicks.
        let css = "#b { position: absolute; left: 0; top: 0; width: 200px; height: 40px; } \
                   #deco { position: absolute; left: 0; top: 0; width: 200px; height: 40px; pointer-events: none; } \
                   #grip { position: absolute; left: 150px; top: 0; width: 50px; height: 40px; pointer-events: auto; }";
        let html =
            "<div id=page><button id=b>Go</button><div id=deco><div id=grip></div></div></div>";
        assert_eq!(
            hit_with_clips(html, css, 10.0, 10.0)[1].as_deref(),
            Some("b")
        );
        assert_eq!(
            hit_with_clips(html, css, 160.0, 10.0)[1].as_deref(),
            Some("grip")
        );
    }

    #[test]
    fn hit_test_follows_paint_order_for_z_index() {
        // Step 1b-z: #top comes first in the document but paints above
        // #later (z-index) — the hit test now agrees with the screen, and
        // its path still names the DOM position of the box it picked.
        let css = "#top { position: absolute; left: 0; top: 0; width: 100px; height: 50px; z-index: 5; } \
                   #later { position: relative; width: 100px; height: 50px; } \
                   #neg { position: absolute; left: 0; top: 60px; width: 100px; height: 50px; z-index: -1; } \
                   #flow { margin-top: 10px; height: 50px; }";
        let html = "<div id=page><div id=top></div><div id=later></div><div id=neg></div><div id=flow></div></div>";
        assert_eq!(
            hit_with_clips(html, css, 10.0, 10.0)[1].as_deref(),
            Some("top")
        );
        // A negative z-index paints under in-flow content.
        assert_eq!(
            hit_with_clips(html, css, 10.0, 75.0)[1].as_deref(),
            Some("flow")
        );
        let document = html::parse(html).unwrap();
        let root = document.roots()[0];
        let sheet = css::parse(css).unwrap();
        let styled = style::style_tree(&document, root, &[sheet]);
        let layout_root = layout::layout_tree(&styled, 400.0);
        let input = input::WindowInput {
            mouse_position: Some((10.0, 10.0 + CHROME_HEIGHT)),
            ..input::WindowInput::default()
        };
        let hit =
            compute_hovered_hit(&input, &layout_root, &layout::ClipTable::default(), 0.0).unwrap();
        assert_eq!(hit.path, vec![0], "the layout path of #top");
    }

    #[test]
    fn a_z_index_auto_positioned_box_makes_no_stacking_context() {
        // Step 1b-z′ (CSS 2.1 appendix E): #wrap is positioned with
        // `z-index: auto`, so #btns (z 2) stacks in #sec's context next to
        // #title (z 2) and, coming later in the document, paints on top —
        // yna's carousel buttons. With `z-index: 0` #wrap is a context of
        // its own at 0 and #title (2) covers everything in it.
        let html = "<div id=page><section id=sec><header id=title>Title</header>\
                    <div id=wrap><div id=btns></div></div></section></div>";
        let css = "#sec { position: relative; } \
                   #title { position: relative; z-index: 2; height: 40px; } \
                   #wrap { position: relative; } \
                   #btns { position: absolute; left: 0; top: -40px; width: 100px; height: 40px; z-index: 2; }";
        assert_eq!(
            hit_with_clips(html, css, 10.0, 10.0)[1].as_deref(),
            Some("btns")
        );
        let own_context = format!("{css} #wrap {{ z-index: 0; }}");
        assert_eq!(
            hit_with_clips(html, &own_context, 10.0, 10.0)[1].as_deref(),
            Some("title")
        );
        // The painter agrees: #btns' fill comes after #title's text.
        let view = page_view(
            &format!("<html><body>{html}</body></html>"),
            &format!("{css} #btns {{ background-color: #0000ff; }}"),
        );
        let mut order = Vec::new();
        render::walk_commands(&view.commands, &mut |c| match c {
            DisplayCommand::Text(t) if t.text.trim() == "Title" => order.push("title"),
            DisplayCommand::SolidRect(color, _) if color.b == 255 && color.r == 0 => {
                order.push("btns")
            }
            _ => {}
        });
        assert_eq!(order, ["title", "btns"]);
    }

    #[test]
    fn click_outside_clip_misses() {
        // (150, 5) is inside the child's box but outside the 100px window.
        let css = "#w { width: 100px; height: 20px; overflow: hidden; } #k { width: 300px; height: 20px; }";
        let html = "<div id=page><div id=w><div id=k></div></div></div>";
        assert_eq!(
            hit_with_clips(html, css, 150.0, 5.0),
            [Some("k".to_string()), Some("page".to_string())]
        );
        // Inside the window the child is still hit.
        assert_eq!(
            hit_with_clips(html, css, 50.0, 5.0)[1].as_deref(),
            Some("k")
        );
    }

    #[test]
    fn clipped_parent_still_lets_an_absolute_child_be_hit() {
        // The parent's own record is skipped, its descendants are still
        // tested: an absolute child outside a static overflow box isn't
        // clipped by it.
        let css = "#w { width: 100px; height: 20px; overflow: hidden; } #a { position: absolute; left: 200px; top: 0; width: 50px; height: 20px; }";
        let html = "<div id=page style='position:relative'><div id=w><div id=a></div></div></div>";
        assert_eq!(
            hit_with_clips(html, css, 220.0, 5.0)[1].as_deref(),
            Some("a")
        );
    }

    #[test]
    fn document_height_counts_content_overflowing_a_viewport_tall_root() {
        // `html, body {height: 100%}` makes the root box the viewport (1p);
        // the scrollable height is still the content's — like Chrome's
        // `scrollHeight` (step 1q).
        let view = page_view(
            "<html><body><div class=\"tall\">x</div></body></html>",
            "html, body { height: 100%; margin: 0; } .tall { height: 3000px; }",
        );
        assert_eq!(
            view.layout_root.dimensions.content.height, 300.0,
            "root = viewport"
        );
        assert_eq!(view.document_height, 3000.0);
    }

    #[test]
    fn document_height_of_a_short_page_is_its_root_margin_box() {
        // Not the viewport and not the canvas fill: a short page reports its
        // own height (body margins included), with or without a background.
        for css in ["", "body { background-color: #102030; }"] {
            let view = page_view(SHORT_PAGE, css);
            let root = &view.layout_root.dimensions;
            let bottom = root.content.y
                + root.content.height
                + root.padding.bottom
                + root.border.bottom
                + root.margin.bottom;
            assert_eq!(view.document_height, bottom, "{css:?}");
            assert!(
                view.document_height < 100.0,
                "{css:?}: {}",
                view.document_height
            );
        }
    }

    #[test]
    fn body_background_propagates_to_the_whole_canvas() {
        // CSS Backgrounds 3 §2.11.2: html transparent → body's background
        // paints the canvas (whole viewport, not just the body box).
        let view = page_view(SHORT_PAGE, "body { background-color: #102030; }");
        let (color, rect) = canvas_rect(&view).expect("canvas fill first");
        assert_eq!((color.r, color.g, color.b), (0x10, 0x20, 0x30));
        assert_eq!((rect.width, rect.height), (400.0, 300.0));
    }

    #[test]
    fn html_background_wins_over_body_for_the_canvas() {
        let view = page_view(
            SHORT_PAGE,
            "html { background-color: #ff0000; } body { background-color: #00ff00; }",
        );
        let (color, _) = canvas_rect(&view).expect("canvas fill first");
        assert_eq!((color.r, color.g, color.b), (255, 0, 0));
    }

    #[test]
    fn no_canvas_fill_without_a_root_background() {
        let view = page_view(SHORT_PAGE, "p { background-color: #00ff00; }");
        assert!(canvas_rect(&view).is_none(), "{:?}", view.commands.first());
    }

    #[test]
    fn caret_is_emitted_for_focused_input_during_blink_on_phase() {
        // Frame 0 lands in the "on" half of the 30-frame blink cadence,
        // so a focused input must produce exactly one SolidRect command —
        // the 1px black caret. Empty fonts make measure_text_width fall
        // back to its fixed-width estimate, so the assertion is determinist.
        let (layout_root, input_id) = setup(r#"<input type="text" value=""/>"#);
        let commands = caret_commands_for_focused_input(&layout_root, Some(input_id), 0);

        assert_eq!(commands.len(), 1);
        match &commands[0] {
            DisplayCommand::SolidRect(color, rect) => {
                assert_eq!(*color, css::Color::BLACK);
                assert_eq!(rect.width, 1.0);
                // 16px default font + 1px bleed on each side = 18px tall.
                assert_eq!(rect.height, 18.0);
            }
            other => panic!("expected SolidRect caret, got {other:?}"),
        }
    }

    #[test]
    fn caret_offsets_to_end_of_value_text() {
        // The caret sits at content.x + measure_text_width(value). With
        // the empty-fonts fallback (each char 12px @ 16pt), "ab" is 24px
        // wide, so the caret lands 24px to the right of content.x.
        let (layout_root, input_id) = setup(r#"<input type="text" value="ab"/>"#);
        let commands = caret_commands_for_focused_input(&layout_root, Some(input_id), 0);
        let rect = match &commands[0] {
            DisplayCommand::SolidRect(_, rect) => *rect,
            other => panic!("expected caret SolidRect, got {other:?}"),
        };

        // content.x for the input root = padding-left + border-left = 4 + 1 = 5.
        let expected_offset = render::measure_text_width("ab", 16.0);
        assert_eq!(rect.x, 5.0 + expected_offset);
    }

    #[test]
    fn caret_disappears_during_blink_off_phase() {
        // Frames 30..60 are the off half — no SolidRect emitted at all.
        let (layout_root, input_id) = setup(r#"<input type="text"/>"#);
        let commands = caret_commands_for_focused_input(&layout_root, Some(input_id), 30);
        assert!(commands.is_empty());

        // And the cycle resumes at 60 → on again.
        let on_again = caret_commands_for_focused_input(&layout_root, Some(input_id), 60);
        assert_eq!(on_again.len(), 1);
    }

    #[test]
    fn caret_skips_when_no_focus() {
        let (layout_root, _input_id) = setup(r#"<input type="text"/>"#);
        let commands = caret_commands_for_focused_input(&layout_root, None, 0);
        assert!(commands.is_empty());
    }

    #[test]
    fn caret_skips_when_focused_node_is_not_an_input() {
        // Focus pointed at a non-input NodeId → no caret. Uses a fake
        // NodeId guaranteed to not match the input slot.
        let (layout_root, input_id) = setup(r#"<input type="text"/>"#);
        let bogus_id = NodeId::from_raw(input_id.raw().wrapping_add(99));
        let commands = caret_commands_for_focused_input(&layout_root, Some(bogus_id), 0);
        assert!(commands.is_empty());
    }

    // Textarea-side variant of `setup`. The walker mirrors find_input_node_id
    // but matches `<textarea>` elements, so multi-line caret tests don't
    // accidentally pick up an unrelated `<input>` from the same fixture.
    fn setup_textarea(html_source: &str) -> (layout::LayoutBox, NodeId) {
        let document = html::parse(html_source).unwrap();
        let root = document.roots()[0];
        let styled = style::style_tree(&document, root, &[]);
        let layout_root = layout::layout_tree(&styled, 400.0);
        let textarea_id = find_textarea_node_id(&document, root)
            .expect("test fixtures must contain at least one <textarea>");
        (layout_root, textarea_id)
    }

    fn find_textarea_node_id(document: &dom::Document, current: NodeId) -> Option<NodeId> {
        let node = document.get(current)?;
        if let NodeType::Element(elem) = &node.node_type
            && elem.tag_name == "textarea"
        {
            return Some(current);
        }
        for child in &node.children {
            if let Some(found) = find_textarea_node_id(document, *child) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn caret_on_textarea_rides_last_line_when_value_has_newlines() {
        // For a `<textarea value="ab\ncd">` the caret must park at the
        // end of "cd", one font-size row below the content origin.
        // That's the contract that lets a freshly-typed Enter park the
        // caret on a blank trailing row, and a typed char after that
        // appear on the new line rather than the previous one.
        let (layout_root, textarea_id) = setup_textarea("<textarea value=\"ab\ncd\"></textarea>");
        let commands = caret_commands_for_focused_input(&layout_root, Some(textarea_id), 0);

        assert_eq!(commands.len(), 1);
        let render::DisplayCommand::SolidRect(_, rect) = &commands[0] else {
            panic!("expected caret SolidRect, got {:?}", commands[0]);
        };
        // x = content.x + measure_text_width("cd")
        let cd_width = render::measure_text_width("cd", 16.0);
        // content.x = padding-left + border-left = 4 + 1 = 5.
        assert_eq!(rect.x, 5.0 + cd_width);
        // Two lines → row index 1 → y offset = font-size * 1 = 16. The
        // -1.0 in the y formula is the visual bleed the chrome caret
        // also uses; matches how the input caret is positioned.
        let content_y = layout_root.dimensions.content.y + 3.0; // padding-top
        // We don't pin the absolute y exactly — the test below
        // exercises the *delta* between lines instead.
        let _ = content_y;
    }

    #[test]
    fn caret_y_advances_by_font_size_per_textarea_line() {
        // Adding extra `\n`s pushes the caret further down by exactly
        // font-size each time. This is the property that makes the
        // `caret rides the trailing line` rule observable independently
        // of any specific content-origin math.
        let (one_line_root, one_line_id) = setup_textarea("<textarea value=\"hi\"></textarea>");
        let (two_line_root, two_line_id) = setup_textarea("<textarea value=\"hi\n\"></textarea>");

        let one_caret = caret_commands_for_focused_input(&one_line_root, Some(one_line_id), 0);
        let two_caret = caret_commands_for_focused_input(&two_line_root, Some(two_line_id), 0);

        let render::DisplayCommand::SolidRect(_, one_rect) = &one_caret[0] else {
            panic!()
        };
        let render::DisplayCommand::SolidRect(_, two_rect) = &two_caret[0] else {
            panic!()
        };
        // Same font-size (16) → second textarea's caret is exactly one
        // row below the first.
        assert_eq!(two_rect.y - one_rect.y, 16.0);
    }
}
