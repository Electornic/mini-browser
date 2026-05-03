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
    layout, net, render, resource, style, window,
};

#[derive(Debug, Clone)]
pub struct LinkTarget {
    pub href: String,
    pub rect: layout::Rect,
    pub underline: bool,
}

#[derive(Debug, Clone)]
pub struct DocumentView {
    // `commands` are what get painted, `links` are the separately tracked clickable regions.
    // `layout_root` is kept around so post-render hit-testing (e.g. computing :hover paths
    // from the mouse position) can walk the same boxes the painter saw.
    pub commands: Vec<render::DisplayCommand>,
    pub links: Vec<LinkTarget>,
    pub layout_root: layout::LayoutBox,
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
    parsed_stylesheet: &css::Stylesheet,
    viewport_width: usize,
    current_url: Option<&net::Url>,
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
    let styled = style::style_tree_with_state(
        parsed_document,
        root,
        std::slice::from_ref(parsed_stylesheet),
        interaction,
    );
    let layout = layout::layout_tree(&styled, viewport_width as f32);
    let mut commands = render::build_display_list(&layout);
    commands.extend(collect_image_commands(&layout, current_url, images));
    let links = collect_link_targets(&layout, None, false, render::Affine::IDENTITY);
    Ok(DocumentView {
        commands,
        links,
        layout_root: layout,
    })
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
        render::DisplayCommand::Text(text) => text.y + text.font_size,
        render::DisplayCommand::Image(image) => image.y + image.height,
        render::DisplayCommand::Gradient(gradient) => gradient.rect.y + gradient.rect.height,
        render::DisplayCommand::BoxShadow(shadow) => shadow.rect.y + shadow.rect.height,
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
        // Inner TransformGroups should never appear in practice; treat as 0.
        render::DisplayCommand::TransformGroup(_, _) => return 0.0,
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
    let mut targets = Vec::new();

    // Link targets are collected separately from display commands because clicking needs rectangles,
    // not just painted pixels.
    if let Some(href) = current_href.filter(|_| should_collect_link_target(layout_box, own_href)) {
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
        });
    }

    for child in &layout_box.children {
        targets.extend(collect_link_targets(
            child,
            current_href,
            no_underline,
            effective_transform,
        ));
    }

    targets
}

fn has_text_decoration_none(layout_box: &layout::LayoutBox) -> bool {
    match &layout_box.box_type {
        layout::BoxType::BlockNode(node)
        | layout::BoxType::FlexNode(node)
        | layout::BoxType::GridNode(node) => matches!(
            node.value("text-decoration"),
            Some(css::Value::Keyword(keyword)) if keyword == "none"
        ),
        layout::BoxType::AnonymousBlock => false,
    }
}

pub fn collect_image_commands(
    layout_box: &layout::LayoutBox,
    base_url: Option<&net::Url>,
    images: &HashMap<String, resource::LoadedImage>,
) -> Vec<render::DisplayCommand> {
    let mut commands = Vec::new();

    if let Some(command) = image_command_for_layout_box(layout_box, base_url, images) {
        commands.push(command);
    }

    for child in &layout_box.children {
        commands.extend(collect_image_commands(child, base_url, images));
    }

    commands
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
            if matches!(styled_node.node_type, NodeType::Text(_))
    )
}

fn href_for_layout_box(layout_box: &layout::LayoutBox) -> Option<&str> {
    match &layout_box.box_type {
        layout::BoxType::BlockNode(styled_node)
        | layout::BoxType::FlexNode(styled_node)
        | layout::BoxType::GridNode(styled_node) => match &styled_node.node_type {
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
        | layout::BoxType::GridNode(styled_node) => match &styled_node.node_type {
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
    base_url: Option<&net::Url>,
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
        pixels: image.pixels.clone(),
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
    input: &window::WindowInput,
    layout_root: &layout::LayoutBox,
    scroll_offset: f32,
) -> Option<Vec<usize>> {
    compute_hovered_hit(input, layout_root, scroll_offset).map(|hit| hit.path)
}

pub fn compute_hovered_hit(
    input: &window::WindowInput,
    layout_root: &layout::LayoutBox,
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

    // Walk the layout tree depth-first, tracking the path of child
    // indices alongside the StyledNode behind each box. Layout child
    // positions mirror DOM child positions (no anonymous boxes are
    // created today), so the path doubles as a DOM path. The deepest
    // containing box wins by virtue of being visited last.
    let mut best: Option<HoverHit> = None;
    let mut path: Vec<usize> = Vec::new();
    walk_for_hover(
        layout_root,
        mouse_x,
        doc_y,
        render::Affine::IDENTITY,
        &mut path,
        &mut best,
    );
    best
}

fn walk_for_hover(
    layout_box: &layout::LayoutBox,
    mouse_x: f32,
    doc_y: f32,
    inherited_transform: render::Affine,
    path: &mut Vec<usize>,
    best: &mut Option<HoverHit>,
) {
    // Compose this box's own `transform` onto the inherited matrix, then map
    // the screen-space cursor back into the box's logical coordinates so the
    // padding-box compare can stay axis-aligned. Pages without `transform`
    // keep the matrix at identity, so the inverse + apply collapse to a no-op.
    let effective_transform = inherited_transform.compose(render::transform_for(layout_box));
    let (logical_x, logical_y) = effective_transform.inverse().apply_point(mouse_x, doc_y);
    let outer = padding_box(layout_box);
    if point_in_rect(logical_x, logical_y, outer) {
        *best = Some(HoverHit {
            path: path.clone(),
            node_id: node_id_for_layout_box(layout_box),
        });
    }
    for (idx, child) in layout_box.children.iter().enumerate() {
        path.push(idx);
        walk_for_hover(child, mouse_x, doc_y, effective_transform, path, best);
        path.pop();
    }
}

fn node_id_for_layout_box(layout_box: &layout::LayoutBox) -> Option<NodeId> {
    match &layout_box.box_type {
        layout::BoxType::BlockNode(node)
        | layout::BoxType::FlexNode(node)
        | layout::BoxType::GridNode(node) => Some(node.node_id),
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
/// `<input>`'s value text. The caret turns on for 30 frames, off for 30,
/// repeating — same `(frame_index / 30).is_multiple_of(2)` cadence the
/// chrome address bar caret uses, so both blinks stay in phase. Returns
/// an empty Vec when no input is focused or when the blink is in its
/// "off" half; callers extend their command list unconditionally and the
/// empty case becomes a no-op paint.
///
/// `focused_node_id` is what links the styled-tree focus state to a
/// specific layout box — `BrowserState::focused_dom_path` resolves to
/// this NodeId on each frame, so the caret follows DOM mutations
/// (renaming, swapping the focused element) without going stale.
pub fn caret_commands_for_focused_input(
    layout_root: &layout::LayoutBox,
    focused_node_id: Option<NodeId>,
    frame_index: usize,
    fonts: &[fontdue::Font],
) -> Vec<render::DisplayCommand> {
    let Some(focused_id) = focused_node_id else {
        return Vec::new();
    };
    if !(frame_index / 30).is_multiple_of(2) {
        return Vec::new();
    }
    let Some(input_box) = find_focused_input_box(layout_root, focused_id) else {
        return Vec::new();
    };

    // Pull the cascaded font-size and current value off the styled node so
    // the caret position uses the SAME font size that paint_self used to
    // draw the value text — otherwise wide-glyph fonts would put the caret
    // in the wrong column.
    let (value, font_size) = match &input_box.box_type {
        layout::BoxType::BlockNode(node)
        | layout::BoxType::FlexNode(node)
        | layout::BoxType::GridNode(node) => {
            let value = match &node.node_type {
                NodeType::Element(elem) => {
                    elem.attributes.get("value").cloned().unwrap_or_default()
                }
                NodeType::Text(_) => return Vec::new(),
            };
            let size = match node.value("font-size") {
                Some(css::Value::Length(v, css::Unit::Px)) => *v,
                _ => 16.0,
            };
            (value, size)
        }
        layout::BoxType::AnonymousBlock => return Vec::new(),
    };

    let caret_offset = render::measure_text_width(&value, font_size, fonts);
    let content = input_box.dimensions.content;
    vec![render::DisplayCommand::SolidRect(
        css::Color::BLACK,
        layout::Rect {
            x: content.x + caret_offset,
            // The chrome caret bleeds 1px above and below the glyph row to
            // give the bar a slightly thicker visual footprint; mirror
            // that here so the page input caret reads the same.
            y: content.y - 1.0,
            width: 1.0,
            height: font_size + 2.0,
        },
    )]
}

// Depth-first search for the layout box that backs the focused <input>.
// We can't pre-compute this from `focused_dom_path` alone because the
// layout box's `node_id` is the canonical identity — DOM mutations between
// the focus event and the next paint can move the same NodeId to a
// different position in the tree (insertBefore, etc.) and the caret has
// to follow.
fn find_focused_input_box(
    box_node: &layout::LayoutBox,
    focused_id: NodeId,
) -> Option<&layout::LayoutBox> {
    let is_focused_input = match &box_node.box_type {
        layout::BoxType::BlockNode(node)
        | layout::BoxType::FlexNode(node)
        | layout::BoxType::GridNode(node) => {
            node.node_id == focused_id
                && matches!(&node.node_type, NodeType::Element(elem) if elem.tag_name == "input")
        }
        layout::BoxType::AnonymousBlock => false,
    };
    if is_focused_input {
        return Some(box_node);
    }
    for child in &box_node.children {
        if let Some(found) = find_focused_input_box(child, focused_id) {
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

            render::DisplayCommand::SolidRect(
                color,
                layout::Rect {
                    x: link.rect.x,
                    y: link.rect.y + link.rect.height.max(1.0) - 1.0,
                    width: link.rect.width.max(1.0),
                    height: 1.0,
                },
            )
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

    #[test]
    fn caret_is_emitted_for_focused_input_during_blink_on_phase() {
        // Frame 0 lands in the "on" half of the 30-frame blink cadence,
        // so a focused input must produce exactly one SolidRect command —
        // the 1px black caret. Empty fonts make measure_text_width fall
        // back to its fixed-width estimate, so the assertion is determinist.
        let (layout_root, input_id) = setup(r#"<input type="text" value=""/>"#);
        let commands = caret_commands_for_focused_input(&layout_root, Some(input_id), 0, &[]);

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
        let commands = caret_commands_for_focused_input(&layout_root, Some(input_id), 0, &[]);
        let rect = match &commands[0] {
            DisplayCommand::SolidRect(_, rect) => *rect,
            other => panic!("expected caret SolidRect, got {other:?}"),
        };

        // content.x for the input root = padding-left + border-left = 4 + 1 = 5.
        let expected_offset = render::measure_text_width("ab", 16.0, &[]);
        assert_eq!(rect.x, 5.0 + expected_offset);
    }

    #[test]
    fn caret_disappears_during_blink_off_phase() {
        // Frames 30..60 are the off half — no SolidRect emitted at all.
        let (layout_root, input_id) = setup(r#"<input type="text"/>"#);
        let commands = caret_commands_for_focused_input(&layout_root, Some(input_id), 30, &[]);
        assert!(commands.is_empty());

        // And the cycle resumes at 60 → on again.
        let on_again = caret_commands_for_focused_input(&layout_root, Some(input_id), 60, &[]);
        assert_eq!(on_again.len(), 1);
    }

    #[test]
    fn caret_skips_when_no_focus() {
        let (layout_root, _input_id) = setup(r#"<input type="text"/>"#);
        let commands = caret_commands_for_focused_input(&layout_root, None, 0, &[]);
        assert!(commands.is_empty());
    }

    #[test]
    fn caret_skips_when_focused_node_is_not_an_input() {
        // Focus pointed at a non-input NodeId → no caret. Uses a fake
        // NodeId guaranteed to not match the input slot.
        let (layout_root, input_id) = setup(r#"<input type="text"/>"#);
        let bogus_id = NodeId::from_raw(input_id.raw().wrapping_add(99));
        let commands = caret_commands_for_focused_input(&layout_root, Some(bogus_id), 0, &[]);
        assert!(commands.is_empty());
    }
}
