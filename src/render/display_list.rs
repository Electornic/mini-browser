// LayoutBox -> DisplayCommand. Walks the layout tree, applies opacity /
// transform inheritance, and emits the per-stacking-context order Chrome's
// painter expects (non-positioned descendants first, then positioned
// descendants ordered by z-index).

use crate::{
    css::{Color, ColorStop, TransformOp, Unit, Value},
    dom::NodeType,
    layout::{Dimensions, LayoutBox, Rect},
};

use super::{
    Affine, CornerRadii, DisplayCommand, GradientCommand, ResolvedStop,
    ShadowCommand, TextCommand,
};

pub fn build_display_list(layout_root: &LayoutBox) -> Vec<DisplayCommand> {
    let mut commands = Vec::new();
    // The root layout box is treated as the initial stacking context: every
    // positioned descendant ends up sorted into z-layers under it. The
    // initial inherited alpha is 1.0 — every non-1 opacity in the tree
    // multiplies into the alpha passed to descendants. The inherited
    // transform starts as identity and accumulates per `transform`
    // declaration on the way down.
    paint_stacking_context(layout_root, &mut commands, 1.0, Affine::IDENTITY);
    commands
}

// Chrome UI and scrolling both reuse this helper to move already-built commands around.
pub fn translate(mut commands: Vec<DisplayCommand>, dx: f32, dy: f32) -> Vec<DisplayCommand> {
    for command in &mut commands {
        match command {
            DisplayCommand::SolidRect(_, rect) => {
                rect.x += dx;
                rect.y += dy;
            }
            DisplayCommand::RoundedRect(_, rect, _) => {
                rect.x += dx;
                rect.y += dy;
            }
            DisplayCommand::Text(text) => {
                text.x += dx;
                text.y += dy;
            }
            DisplayCommand::Image(image) => {
                image.x += dx;
                image.y += dy;
            }
            DisplayCommand::Gradient(gradient) => {
                gradient.rect.x += dx;
                gradient.rect.y += dy;
            }
            DisplayCommand::BoxShadow(shadow) => {
                shadow.rect.x += dx;
                shadow.rect.y += dy;
            }
            DisplayCommand::TransformGroup(transform, _) => {
                // The inner primitives are in logical coords; shifting them
                // means composing a screen-space translate on the *left* of
                // the matrix so the result is `T(dx,dy) * matrix * logical`.
                *transform = Affine::translate(dx, dy).compose(*transform);
            }
        }
    }

    commands
}

fn paint_stacking_context(
    layout_box: &LayoutBox,
    commands: &mut Vec<DisplayCommand>,
    inherited_alpha: f32,
    inherited_transform: Affine,
) {
    let effective_alpha = inherited_alpha * opacity_of(layout_box);
    let effective_transform = inherited_transform.compose(transform_for(layout_box));

    // 1. Paint this stacking-context-creator's own bg/border/text.
    paint_self(layout_box, commands, effective_alpha, effective_transform);

    // Walk descendants and pluck out the positioned subtrees — each becomes
    // its own atomic z-layer. Non-positioned content is painted between the
    // negative and zero/positive z-layers, so we need both groups separately.
    // The alpha and transform stored alongside each entry are the *inherited*
    // values from the actual ancestor chain, before the positioned box's own
    // opacity / own transform are applied — paint_stacking_context applies
    // those itself.
    let mut positioned: Vec<(&LayoutBox, f32, Affine)> = Vec::new();
    for child in &layout_box.children {
        collect_positioned_into(child, &mut positioned, effective_alpha, effective_transform);
    }

    let mut negative: Vec<(&LayoutBox, f32, Affine)> = positioned
        .iter()
        .copied()
        .filter(|(b, _, _)| z_index_of(b) < 0)
        .collect();
    let zero_or_auto: Vec<(&LayoutBox, f32, Affine)> = positioned
        .iter()
        .copied()
        .filter(|(b, _, _)| z_index_of(b) == 0)
        .collect();
    let mut positive: Vec<(&LayoutBox, f32, Affine)> = positioned
        .iter()
        .copied()
        .filter(|(b, _, _)| z_index_of(b) > 0)
        .collect();
    // Stable sort preserves tree order among siblings sharing a z-index.
    negative.sort_by_key(|(b, _, _)| z_index_of(b));
    positive.sort_by_key(|(b, _, _)| z_index_of(b));

    // 2. Negative-z layers paint first → they sit BEHIND non-positioned content.
    for (child, alpha, transform) in &negative {
        paint_stacking_context(child, commands, *alpha, *transform);
    }

    // 3. Non-positioned descendants in tree order. Positioned subtrees are
    //    skipped here because they already belong to a z-layer.
    for child in &layout_box.children {
        paint_non_positioned(child, commands, effective_alpha, effective_transform);
    }

    // 4 & 5. Zero/auto layers in tree order, then positive-z (sorted asc).
    for (child, alpha, transform) in zero_or_auto.iter().chain(positive.iter()) {
        paint_stacking_context(child, commands, *alpha, *transform);
    }
}

fn paint_non_positioned(
    layout_box: &LayoutBox,
    commands: &mut Vec<DisplayCommand>,
    inherited_alpha: f32,
    inherited_transform: Affine,
) {
    // Stop at any positioned box — its painting belongs to its enclosing
    // stacking context, where z-order is decided.
    if is_positioned_box(layout_box) {
        return;
    }
    let effective_alpha = inherited_alpha * opacity_of(layout_box);
    let effective_transform = inherited_transform.compose(transform_for(layout_box));
    paint_self(layout_box, commands, effective_alpha, effective_transform);
    for child in &layout_box.children {
        paint_non_positioned(child, commands, effective_alpha, effective_transform);
    }
}


fn paint_self(
    layout_box: &LayoutBox,
    commands: &mut Vec<DisplayCommand>,
    alpha: f32,
    transform: Affine,
) {
    // Per-box paint order is shadow -> background-color -> background-image
    // (gradient) -> border -> text, matching CSS spec stacking within a
    // single element. Shadow is emitted first so the box's own bg covers
    // the part of the shadow that overlaps with the box.
    let start = commands.len();
    if let Some(command) = shadow_command(layout_box, alpha) {
        commands.push(command);
    }
    if let Some(command) = background_command(layout_box, alpha) {
        commands.push(command);
    }
    if let Some(command) = gradient_command(layout_box, alpha) {
        commands.push(command);
    }
    commands.extend(border_commands(layout_box, alpha));
    if let Some(command) = text_shadow_command(layout_box, alpha) {
        commands.push(command);
    }
    if let Some(command) = text_command(layout_box, alpha) {
        commands.push(command);
    }
    // Push the inherited+own transform onto the commands this box just emitted.
    // The emitters work in logical (untransformed) coordinates; the affine
    // matrix is the only thing that maps logical → screen pixels.
    finalize_box_transform(commands, start, transform);
}


fn collect_positioned_into<'a>(
    layout_box: &'a LayoutBox,
    out: &mut Vec<(&'a LayoutBox, f32, Affine)>,
    inherited_alpha: f32,
    inherited_transform: Affine,
) {
    if is_positioned_box(layout_box) {
        // Stash the *inherited* alpha and *inherited* transform (before this
        // box's own opacity / own transform); the recipient paint_stacking_context
        // applies the box's own contributions itself.
        out.push((layout_box, inherited_alpha, inherited_transform));
        // Don't recurse: the positioned box's own descendants live inside its
        // own stacking context and are handled when we paint it.
        return;
    }
    let effective_alpha = inherited_alpha * opacity_of(layout_box);
    let effective_transform = inherited_transform.compose(transform_for(layout_box));
    for child in &layout_box.children {
        collect_positioned_into(child, out, effective_alpha, effective_transform);
    }
}


pub fn transform_for(layout_box: &LayoutBox) -> Affine {
    // Compose every `transform: ...` op of *this* box into a single affine.
    // For commit 1 the only op is translate; the wrapping with the box centre
    // is a no-op for translates (they commute with the conjugating translates),
    // but it is exactly what scale/rotate need so it lands correctly here.
    let node = match layout_box.styled_node() {
        Some(node) => node,
        None => return Affine::IDENTITY,
    };
    let ops = match node.value("transform") {
        Some(Value::TransformList(ops)) => ops,
        _ => return Affine::IDENTITY,
    };
    if ops.is_empty() {
        return Affine::IDENTITY;
    }
    let mut raw = Affine::IDENTITY;
    for op in ops {
        let m = match op {
            TransformOp::Translate { x, y } => Affine::translate(*x, *y),
            TransformOp::Scale { x, y } => Affine::scale(*x, *y),
            TransformOp::Rotate(theta) => Affine::rotate(*theta),
        };
        raw = raw.compose(m);
    }
    let border = layout_box.dimensions.border_box();
    let cx = border.x + border.width * 0.5;
    let cy = border.y + border.height * 0.5;
    Affine::translate(cx, cy)
        .compose(raw)
        .compose(Affine::translate(-cx, -cy))
}


fn finalize_box_transform(commands: &mut Vec<DisplayCommand>, start: usize, transform: Affine) {
    // Skipping the work when there is nothing to do keeps the painted output
    // bit-identical for pages that do not use `transform`.
    if transform.is_identity() {
        return;
    }
    if transform.is_axis_aligned() {
        // Translate+scale: bake into rect/x/y/width/height in place. The
        // rasterizer's existing fast path then handles the result without
        // needing to know a transform was ever involved.
        bake_axis_aligned(&mut commands[start..], transform);
        return;
    }
    // Rotation/shear: pull this box's just-emitted primitives into a single
    // group so the rasterizer can scan-convert them through the matrix
    // (inverse-pixel-sample). The rect/x/y inside stay logical.
    let inner: Vec<DisplayCommand> = commands.drain(start..).collect();
    commands.push(DisplayCommand::TransformGroup(transform, inner));
}


fn bake_axis_aligned(commands: &mut [DisplayCommand], transform: Affine) {
    for command in commands {
        match command {
            DisplayCommand::SolidRect(_, rect) => transform_rect_in_place(rect, transform),
            DisplayCommand::RoundedRect(_, rect, _) => transform_rect_in_place(rect, transform),
            DisplayCommand::Text(text) => {
                let (x, y) = transform.apply_point(text.x, text.y);
                text.x = x;
                text.y = y;
                // Text scales with the diagonal of the matrix. For uniform
                // scale a == d so this is exact; non-uniform scale is a
                // compromise (real CSS would distort glyphs along one axis,
                // which the bitmap rasterizer here doesn't do).
                text.font_size *= scalar_scale(transform);
            }
            DisplayCommand::Image(image) => {
                let (x, y) = transform.apply_point(image.x, image.y);
                image.x = x;
                image.y = y;
                image.width *= transform.a;
                image.height *= transform.d;
            }
            DisplayCommand::Gradient(gradient) => {
                transform_rect_in_place(&mut gradient.rect, transform)
            }
            DisplayCommand::BoxShadow(shadow) => {
                transform_rect_in_place(&mut shadow.rect, transform);
                // Blur falloff travels with the box, so its pixel-space
                // size has to scale with the matrix too — otherwise a 4x
                // scaled card keeps a tiny shadow that no longer reads
                // as soft light.
                shadow.blur_radius *= scalar_scale(transform);
            }
            // Rotated children should not be present here because rotation
            // routes through the TransformGroup branch; ignore defensively.
            DisplayCommand::TransformGroup(_, _) => {}
        }
    }
}


fn scalar_scale(transform: Affine) -> f32 {
    // The matrix is axis-aligned for translate+scale (b == c == 0), so the
    // average of the two diagonal entries is the obvious "scalar" scale to
    // apply to inherently 1-dimensional things like font-size and blur.
    (transform.a + transform.d) * 0.5
}


fn transform_rect_in_place(rect: &mut Rect, transform: Affine) {
    // Translate+scale transforms keep an axis-aligned rect axis-aligned,
    // so we apply the matrix to the origin and the diagonal scale entries
    // to the dimensions. When rotate lands the rect will need to widen
    // into a quad and the rasterizer will pick up polygon scan-conversion.
    let (x, y) = transform.apply_point(rect.x, rect.y);
    rect.x = x;
    rect.y = y;
    rect.width *= transform.a;
    rect.height *= transform.d;
    // Negative scale flips the rect across its origin; normalise so the
    // existing axis-aligned fillers (which assume positive width/height)
    // still hit the same screen pixels.
    if rect.width < 0.0 {
        rect.x += rect.width;
        rect.width = -rect.width;
    }
    if rect.height < 0.0 {
        rect.y += rect.height;
        rect.height = -rect.height;
    }
}


fn is_positioned_box(layout_box: &LayoutBox) -> bool {
    let node = match layout_box.styled_node() {
        Some(node) => node,
        None => return false,
    };
    matches!(
        node.value("position"),
        Some(Value::Keyword(keyword))
            if keyword == "relative" || keyword == "absolute" || keyword == "fixed"
    )
}

fn opacity_of(layout_box: &LayoutBox) -> f32 {
    // CSS `opacity` is a unitless number in [0, 1]; default 1 (fully opaque).
    // Real CSS would isolate the subtree as a compositing group; we cheat by
    // multiplying the factor into every emitted color along the descendant
    // chain — visually identical for solid fills, slightly off for overlapping
    // children that should composite together first.
    let node = match layout_box.styled_node() {
        Some(node) => node,
        None => return 1.0,
    };
    match node.value("opacity") {
        Some(Value::Number(value)) => value.clamp(0.0, 1.0),
        _ => 1.0,
    }
}

fn apply_alpha(color: Color, factor: f32) -> Color {
    Color {
        r: color.r,
        g: color.g,
        b: color.b,
        a: ((color.a as f32) * factor).clamp(0.0, 255.0) as u8,
    }
}

fn z_index_of(layout_box: &LayoutBox) -> i32 {
    // `z-index: auto` is treated as 0 here. Per CSS spec, auto and 0 differ
    // (auto does not create a stacking context), but this toy treats every
    // positioned box as a context, so they share the same layer in practice.
    let node = match layout_box.styled_node() {
        Some(node) => node,
        None => return 0,
    };
    match node.value("z-index") {
        Some(Value::Number(value)) => *value as i32,
        Some(Value::Length(value, _)) => *value as i32,
        _ => 0,
    }
}

fn shadow_command(layout_box: &LayoutBox, alpha: f32) -> Option<DisplayCommand> {
    let node = layout_box.styled_node()?;
    let shadow = match node.value("box-shadow") {
        Some(Value::BoxShadow(shadow)) => *shadow,
        _ => return None,
    };
    // The shadow is anchored to the *border* box, then offset and inflated by
    // the spread. Negative spread shrinks; positive spread grows symmetrically.
    let border = layout_box.dimensions.border_box();
    let rect = Rect {
        x: border.x + shadow.offset_x - shadow.spread_radius,
        y: border.y + shadow.offset_y - shadow.spread_radius,
        width: (border.width + 2.0 * shadow.spread_radius).max(0.0),
        height: (border.height + 2.0 * shadow.spread_radius).max(0.0),
    };
    Some(DisplayCommand::BoxShadow(ShadowCommand {
        rect,
        blur_radius: shadow.blur_radius,
        color: apply_alpha(shadow.color, alpha),
    }))
}

fn background_command(layout_box: &LayoutBox, alpha: f32) -> Option<DisplayCommand> {
    let node = layout_box.styled_node()?;
    let color = match node.value("background-color") {
        Some(Value::Color(color)) => apply_alpha(*color, alpha),
        _ => return None,
    };

    let rect = layout_box.dimensions.padding_box();
    let radii = border_radii(node);
    if radii.tl == 0.0 && radii.tr == 0.0 && radii.br == 0.0 && radii.bl == 0.0 {
        Some(DisplayCommand::SolidRect(color, rect))
    } else {
        Some(DisplayCommand::RoundedRect(color, rect, radii))
    }
}

fn gradient_command(layout_box: &LayoutBox, alpha: f32) -> Option<DisplayCommand> {
    let node = layout_box.styled_node()?;
    let gradient = match node.value("background-image") {
        Some(Value::Gradient(gradient)) => gradient,
        _ => return None,
    };
    let stops = resolve_gradient_stops(&gradient.stops, alpha);
    if stops.len() < 2 {
        return None;
    }
    Some(DisplayCommand::Gradient(GradientCommand {
        rect: layout_box.dimensions.padding_box(),
        kind: gradient.kind,
        stops,
    }))
}

fn resolve_gradient_stops(stops: &[ColorStop], alpha: f32) -> Vec<ResolvedStop> {
    // CSS auto-position rules (simplified):
    //   1. First stop without a position pins to 0.0; last pins to 1.0.
    //   2. Any unpositioned stop between two positioned ones is filled in by
    //      even distribution along the gap.
    //   3. After the pass, positions are clamped to be monotonically
    //      non-decreasing so a malformed gradient never produces NaN math.
    let n = stops.len();
    if n == 0 {
        return Vec::new();
    }
    let mut positions: Vec<Option<f32>> = stops.iter().map(|stop| stop.position).collect();
    if positions[0].is_none() {
        positions[0] = Some(0.0);
    }
    if positions[n - 1].is_none() {
        positions[n - 1] = Some(1.0);
    }
    let mut last_known = 0;
    for i in 1..n {
        if positions[i].is_some() {
            // Distribute every still-unknown stop in (last_known, i) evenly.
            let start = positions[last_known].unwrap();
            let end = positions[i].unwrap();
            let span = i - last_known;
            for offset in 1..span {
                if positions[last_known + offset].is_none() {
                    let t = offset as f32 / span as f32;
                    positions[last_known + offset] = Some(start + (end - start) * t);
                }
            }
            last_known = i;
        }
    }
    let mut last = 0.0;
    let mut out = Vec::with_capacity(n);
    for (i, position) in positions.iter().enumerate() {
        let mut p = position.unwrap_or(last);
        if p < last {
            p = last;
        }
        last = p;
        out.push(ResolvedStop {
            position: p,
            color: apply_alpha(stops[i].color, alpha),
        });
    }
    out
}

fn border_radii(node: &crate::style::StyledNode) -> CornerRadii {
    // The shorthand `border-radius` is expanded into four corner properties at parse time,
    // so painting just reads each corner independently.
    CornerRadii {
        tl: corner_radius(node, "border-top-left-radius"),
        tr: corner_radius(node, "border-top-right-radius"),
        br: corner_radius(node, "border-bottom-right-radius"),
        bl: corner_radius(node, "border-bottom-left-radius"),
    }
}

fn corner_radius(node: &crate::style::StyledNode, name: &str) -> f32 {
    match node.value(name) {
        Some(Value::Length(value, Unit::Px)) => *value,
        _ => 0.0,
    }
}

fn text_shadow_command(layout_box: &LayoutBox, alpha: f32) -> Option<DisplayCommand> {
    // Only the actual text-bearing layout box paints a shadow — Element
    // boxes never carry text on their own, so they short-circuit here.
    let node = layout_box.styled_node()?;
    let text = match &node.node_type {
        NodeType::Text(text) => text.clone(),
        NodeType::Element(_) => return None,
    };
    let shadow = match node.value("text-shadow") {
        Some(Value::TextShadow(shadow)) => *shadow,
        _ => return None,
    };

    let glyph_size = font_size(node);
    let half_leading = ((layout_box.dimensions.content.height - glyph_size) / 2.0).max(0.0);

    // The shadow is just a second Text command at the offset position with
    // the shadow color. blur_radius is parsed but ignored for now — proper
    // glyph blur would need a per-glyph rasterize-then-blur pass.
    Some(DisplayCommand::Text(TextCommand {
        text,
        x: layout_box.dimensions.content.x + shadow.offset_x,
        y: layout_box.dimensions.content.y + half_leading + shadow.offset_y,
        color: apply_alpha(shadow.color, alpha),
        font_size: glyph_size,
    }))
}

fn text_command(layout_box: &LayoutBox, alpha: f32) -> Option<DisplayCommand> {
    let node = layout_box.styled_node()?;
    let text = match &node.node_type {
        NodeType::Text(text) => text.clone(),
        NodeType::Element(_) => return None,
    };

    // CSS half-leading: when line-height > font-size, the extra space splits
    // evenly above and below the glyph so the text sits centered inside its
    // line box. This is what makes `line-height: 2` look balanced rather than
    // pushing the glyph to the top of an oversized box.
    let glyph_size = font_size(node);
    let half_leading = ((layout_box.dimensions.content.height - glyph_size) / 2.0).max(0.0);

    Some(DisplayCommand::Text(TextCommand {
        text,
        x: layout_box.dimensions.content.x,
        y: layout_box.dimensions.content.y + half_leading,
        color: apply_alpha(text_color(node), alpha),
        font_size: glyph_size,
    }))
}

fn border_commands(layout_box: &LayoutBox, alpha: f32) -> Vec<DisplayCommand> {
    let node = match layout_box.styled_node() {
        Some(node) => node,
        None => return Vec::new(),
    };
    let color = match node.value("border-color") {
        Some(Value::Color(color)) => apply_alpha(*color, alpha),
        _ => return Vec::new(),
    };
    let border = layout_box.dimensions.border;
    if border.left == 0.0 && border.right == 0.0 && border.top == 0.0 && border.bottom == 0.0 {
        return Vec::new();
    }

    let border_box = layout_box.dimensions.border_box();
    let mut commands = Vec::new();

    if border.top > 0.0 {
        commands.push(DisplayCommand::SolidRect(
            color,
            Rect {
                x: border_box.x,
                y: border_box.y,
                width: border_box.width,
                height: border.top,
            },
        ));
    }

    if border.bottom > 0.0 {
        commands.push(DisplayCommand::SolidRect(
            color,
            Rect {
                x: border_box.x,
                y: border_box.y + border_box.height - border.bottom,
                width: border_box.width,
                height: border.bottom,
            },
        ));
    }

    if border.left > 0.0 {
        commands.push(DisplayCommand::SolidRect(
            color,
            Rect {
                x: border_box.x,
                y: border_box.y,
                width: border.left,
                height: border_box.height,
            },
        ));
    }

    if border.right > 0.0 {
        commands.push(DisplayCommand::SolidRect(
            color,
            Rect {
                x: border_box.x + border_box.width - border.right,
                y: border_box.y,
                width: border.right,
                height: border_box.height,
            },
        ));
    }

    commands
}

fn text_color(node: &crate::style::StyledNode) -> Color {
    match node.value("color") {
        Some(Value::Color(color)) => *color,
        _ => Color::BLACK,
    }
}

fn font_size(node: &crate::style::StyledNode) -> f32 {
    match node.value("font-size") {
        Some(Value::Length(value, Unit::Px)) => *value,
        _ => 16.0,
    }
}

impl LayoutBox {
    fn styled_node(&self) -> Option<&crate::style::StyledNode> {
        match &self.box_type {
            crate::layout::BoxType::BlockNode(node)
            | crate::layout::BoxType::FlexNode(node)
            | crate::layout::BoxType::GridNode(node) => Some(node),
            crate::layout::BoxType::AnonymousBlock => None,
        }
    }
}

impl Dimensions {
    fn padding_box(&self) -> Rect {
        Rect {
            x: self.content.x - self.padding.left,
            y: self.content.y - self.padding.top,
            width: self.content.width + self.padding.left + self.padding.right,
            height: self.content.height + self.padding.top + self.padding.bottom,
        }
    }

    fn border_box(&self) -> Rect {
        let padding_box = self.padding_box();
        Rect {
            x: padding_box.x - self.border.left,
            y: padding_box.y - self.border.top,
            width: padding_box.width + self.border.left + self.border.right,
            height: padding_box.height + self.border.top + self.border.bottom,
        }
    }
}
