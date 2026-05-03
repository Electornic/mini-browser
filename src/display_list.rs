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
