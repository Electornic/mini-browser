use crate::{
    css::{Color, ColorStop, GradientDirection, GradientKind, TransformOp, Unit, Value},
    dom::NodeType,
    layout::{Dimensions, LayoutBox, Rect},
};

/// 2-D affine transform stored as the six matrix entries of
/// ```text
/// | a c e |
/// | b d f |
/// | 0 0 1 |
/// ```
/// `apply_point` premultiplies a column vector `(x, y, 1)`. `compose` matches
/// CSS semantics: `parent.compose(child).apply_point(p)` is the same as
/// `parent.apply_point(child.apply_point(p))`, so transforms inherit naturally
/// down the tree just like `inherited_alpha`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Affine {
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub fn translate(tx: f32, ty: f32) -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: ty,
        }
    }

    /// Standard 3x3 matrix multiply, restricted to the affine submatrix.
    pub fn compose(&self, other: Self) -> Self {
        Self {
            a: self.a * other.a + self.c * other.b,
            b: self.b * other.a + self.d * other.b,
            c: self.a * other.c + self.c * other.d,
            d: self.b * other.c + self.d * other.d,
            e: self.a * other.e + self.c * other.f + self.e,
            f: self.b * other.e + self.d * other.f + self.f,
        }
    }

    pub fn apply_point(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// Strict equality is fine because every non-identity matrix in the system
    /// is built from explicit constructors — no floating-point drift sneaks in
    /// when the page declares no transform at all.
    pub fn is_identity(&self) -> bool {
        self.a == 1.0
            && self.b == 0.0
            && self.c == 0.0
            && self.d == 1.0
            && self.e == 0.0
            && self.f == 0.0
    }

    /// Returns the matrix that undoes this one, or identity if the linear part
    /// is degenerate (zero determinant). The hit-test path uses this to map a
    /// screen-space cursor back into the layout-tree's logical coordinates so
    /// it can compare against the un-transformed `padding_box` rectangles.
    pub fn inverse(&self) -> Self {
        let det = self.a * self.d - self.b * self.c;
        if det == 0.0 {
            return Self::IDENTITY;
        }
        let inv_det = 1.0 / det;
        let a = self.d * inv_det;
        let b = -self.b * inv_det;
        let c = -self.c * inv_det;
        let d = self.a * inv_det;
        let e = -(a * self.e + c * self.f);
        let f = -(b * self.e + d * self.f);
        Self { a, b, c, d, e, f }
    }
}

// Rendering is two-stage: layout boxes become display commands, then commands rasterize to pixels.
#[derive(Debug, Clone, PartialEq)]
pub enum DisplayCommand {
    SolidRect(Color, Rect),
    RoundedRect(Color, Rect, CornerRadii),
    Text(TextCommand),
    Image(ImageCommand),
    /// Linear or radial gradient fill. Stops are pre-resolved to absolute
    /// positions in 0..1 along the gradient axis so the rasterizer doesn't
    /// have to redo the auto-position math.
    Gradient(GradientCommand),
    /// Outset box-shadow: a colored rectangle (already shifted by offset and
    /// inflated by spread) with a linear-ramp blur band around the edges.
    BoxShadow(ShadowCommand),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowCommand {
    pub rect: Rect,
    pub blur_radius: f32,
    pub color: Color,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GradientCommand {
    pub rect: Rect,
    pub kind: GradientKind,
    pub stops: Vec<ResolvedStop>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedStop {
    pub position: f32,
    pub color: Color,
}

// Per-corner radii so tabs (top corners only) and pills (uniform) share one primitive.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CornerRadii {
    pub tl: f32,
    pub tr: f32,
    pub br: f32,
    pub bl: f32,
}

impl CornerRadii {
    pub fn uniform(radius: f32) -> Self {
        Self {
            tl: radius,
            tr: radius,
            br: radius,
            bl: radius,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextCommand {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub color: Color,
    pub font_size: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageCommand {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub source_width: usize,
    pub source_height: usize,
    pub pixels: Vec<u32>,
}

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
        }
    }

    commands
}

pub fn rasterize(
    commands: &[DisplayCommand],
    width: usize,
    height: usize,
    fonts: &[fontdue::Font],
) -> Vec<u32> {
    let mut buffer = vec![rgb_u32(Color::WHITE); width * height];

    for command in commands {
        match command {
            DisplayCommand::SolidRect(color, rect) => {
                fill_rect(&mut buffer, width, height, *color, *rect)
            }
            DisplayCommand::RoundedRect(color, rect, radii) => {
                fill_rounded_rect(&mut buffer, width, height, *color, *rect, *radii)
            }
            DisplayCommand::Text(text) => draw_text(&mut buffer, width, height, text, fonts),
            DisplayCommand::Image(image) => draw_image(&mut buffer, width, height, image),
            DisplayCommand::Gradient(gradient) => {
                fill_gradient(&mut buffer, width, height, gradient)
            }
            DisplayCommand::BoxShadow(shadow) => {
                fill_box_shadow(&mut buffer, width, height, shadow)
            }
        }
    }

    buffer
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
    apply_transform(&mut commands[start..], transform);
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

fn apply_transform(commands: &mut [DisplayCommand], transform: Affine) {
    // Skipping the walk when there is nothing to do keeps the painted output
    // bit-identical for pages that do not use `transform`.
    if transform.is_identity() {
        return;
    }
    for command in commands {
        match command {
            DisplayCommand::SolidRect(_, rect) => transform_rect_in_place(rect, transform),
            DisplayCommand::RoundedRect(_, rect, _) => transform_rect_in_place(rect, transform),
            DisplayCommand::Text(text) => {
                let (x, y) = transform.apply_point(text.x, text.y);
                text.x = x;
                text.y = y;
            }
            DisplayCommand::Image(image) => {
                let (x, y) = transform.apply_point(image.x, image.y);
                image.x = x;
                image.y = y;
            }
            DisplayCommand::Gradient(gradient) => {
                transform_rect_in_place(&mut gradient.rect, transform)
            }
            DisplayCommand::BoxShadow(shadow) => {
                transform_rect_in_place(&mut shadow.rect, transform)
            }
        }
    }
}

fn transform_rect_in_place(rect: &mut Rect, transform: Affine) {
    // Translate-only transforms keep an axis-aligned rect axis-aligned, so
    // we only have to push the origin through the matrix. When scale and
    // rotate land later, the rect itself will need to grow into a quad.
    let (x, y) = transform.apply_point(rect.x, rect.y);
    rect.x = x;
    rect.y = y;
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
    let text = match &node.node.node_type {
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
    let text = match &node.node.node_type {
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
            crate::layout::BoxType::BlockNode(node) => Some(node),
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

impl Color {
    pub const WHITE: Self = Self {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };

    pub const BLACK: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
}

fn fill_rect(buffer: &mut [u32], width: usize, height: usize, color: Color, rect: Rect) {
    let x_start = rect.x.max(0.0).floor() as usize;
    let y_start = rect.y.max(0.0).floor() as usize;
    let x_end = (rect.x + rect.width).ceil().max(0.0) as usize;
    let y_end = (rect.y + rect.height).ceil().max(0.0) as usize;
    let x_end = x_end.min(width);
    let y_end = y_end.min(height);

    if color.a == 0 {
        return;
    }
    if color.a == 255 {
        // Fully opaque: skip the per-pixel blend math.
        let pixel = rgb_u32(color);
        for y in y_start..y_end {
            let row = y * width;
            for x in x_start..x_end {
                buffer[row + x] = pixel;
            }
        }
        return;
    }

    // Source-over with the fill color's alpha as the blend weight against
    // whatever is already in the buffer at each pixel.
    let a = color.a as u32;
    let inv = 255 - a;
    let cr = color.r as u32;
    let cg = color.g as u32;
    let cb = color.b as u32;
    for y in y_start..y_end {
        let row = y * width;
        for x in x_start..x_end {
            let bg = buffer[row + x];
            let r = (a * cr + inv * ((bg >> 16) & 0xFF)) / 255;
            let g = (a * cg + inv * ((bg >> 8) & 0xFF)) / 255;
            let b = (a * cb + inv * (bg & 0xFF)) / 255;
            buffer[row + x] = (r << 16) | (g << 8) | b;
        }
    }
}

fn fill_box_shadow(buffer: &mut [u32], width: usize, height: usize, shadow: &ShadowCommand) {
    if shadow.color.a == 0 {
        return;
    }
    if shadow.rect.width <= 0.0 || shadow.rect.height <= 0.0 {
        return;
    }

    let blur = shadow.blur_radius;
    // Affected region = shadow rect inflated by `blur` on every side. Anything
    // farther than `blur` from the rect edge has zero coverage.
    let x_start = (shadow.rect.x - blur).max(0.0).floor() as usize;
    let y_start = (shadow.rect.y - blur).max(0.0).floor() as usize;
    let x_end = (((shadow.rect.x + shadow.rect.width + blur).ceil()).max(0.0) as usize).min(width);
    let y_end =
        (((shadow.rect.y + shadow.rect.height + blur).ceil()).max(0.0) as usize).min(height);

    let left = shadow.rect.x;
    let top = shadow.rect.y;
    let right = shadow.rect.x + shadow.rect.width;
    let bottom = shadow.rect.y + shadow.rect.height;

    for y in y_start..y_end {
        for x in x_start..x_end {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            // Distance from this pixel to the *closest* point inside the
            // shadow rect. Pixels inside the rect score zero distance and
            // therefore full coverage — outside, the linear ramp falls off
            // over `blur` distance and clamps to 0 beyond that.
            let dx = (left - px).max(0.0).max(px - right);
            let dy = (top - py).max(0.0).max(py - bottom);
            let dist = (dx * dx + dy * dy).sqrt();
            let coverage = if blur > 0.0 {
                (1.0 - dist / blur).clamp(0.0, 1.0)
            } else if dist == 0.0 {
                1.0
            } else {
                0.0
            };
            let combined_alpha = ((shadow.color.a as f32) * coverage) as u32;
            if combined_alpha == 0 {
                continue;
            }

            let idx = y * width + x;
            let bg = buffer[idx];
            let inv = 255 - combined_alpha;
            let r = (combined_alpha * shadow.color.r as u32 + inv * ((bg >> 16) & 0xFF)) / 255;
            let g = (combined_alpha * shadow.color.g as u32 + inv * ((bg >> 8) & 0xFF)) / 255;
            let b = (combined_alpha * shadow.color.b as u32 + inv * (bg & 0xFF)) / 255;
            buffer[idx] = (r << 16) | (g << 8) | b;
        }
    }
}

fn fill_gradient(buffer: &mut [u32], width: usize, height: usize, gradient: &GradientCommand) {
    let rect = gradient.rect;
    let x_start = rect.x.max(0.0).floor() as usize;
    let y_start = rect.y.max(0.0).floor() as usize;
    let x_end = ((rect.x + rect.width).ceil().max(0.0) as usize).min(width);
    let y_end = ((rect.y + rect.height).ceil().max(0.0) as usize).min(height);

    if gradient.stops.is_empty() {
        return;
    }
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }

    // Radial uses the ellipse with semi-axes = half the rect, centered on the
    // padding box. Sampling each pixel reduces to normalised distance from the
    // centre, which already lies in the same 0..1 progress space the linear
    // path uses, so stop sampling and source-over blending are shared.
    let cx = rect.x + rect.width * 0.5;
    let cy = rect.y + rect.height * 0.5;
    let rx = rect.width * 0.5;
    let ry = rect.height * 0.5;

    for y in y_start..y_end {
        for x in x_start..x_end {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let progress = match gradient.kind {
                GradientKind::Linear(GradientDirection::ToBottom) => (py - rect.y) / rect.height,
                GradientKind::Linear(GradientDirection::ToTop) => 1.0 - (py - rect.y) / rect.height,
                GradientKind::Linear(GradientDirection::ToRight) => (px - rect.x) / rect.width,
                GradientKind::Linear(GradientDirection::ToLeft) => 1.0 - (px - rect.x) / rect.width,
                GradientKind::Radial => {
                    let nx = (px - cx) / rx;
                    let ny = (py - cy) / ry;
                    (nx * nx + ny * ny).sqrt()
                }
            };
            let progress = progress.clamp(0.0, 1.0);
            let color = sample_gradient(&gradient.stops, progress);
            if color.a == 0 {
                continue;
            }
            let idx = y * width + x;
            if color.a == 255 {
                buffer[idx] = rgb_u32(color);
            } else {
                let bg = buffer[idx];
                let a = color.a as u32;
                let inv = 255 - a;
                let r = (a * color.r as u32 + inv * ((bg >> 16) & 0xFF)) / 255;
                let g = (a * color.g as u32 + inv * ((bg >> 8) & 0xFF)) / 255;
                let b = (a * color.b as u32 + inv * (bg & 0xFF)) / 255;
                buffer[idx] = (r << 16) | (g << 8) | b;
            }
        }
    }
}

fn sample_gradient(stops: &[ResolvedStop], progress: f32) -> Color {
    // Clamp to the first/last stop for progress outside the [0, 1] band.
    if progress <= stops[0].position {
        return stops[0].color;
    }
    let last = stops[stops.len() - 1];
    if progress >= last.position {
        return last.color;
    }
    // Linear search is fine for the small stop counts CSS gradients have in
    // practice — interpolate the bracketing pair in straight RGB space.
    for window in stops.windows(2) {
        let a = window[0];
        let b = window[1];
        if progress >= a.position && progress <= b.position {
            let span = (b.position - a.position).max(f32::EPSILON);
            let t = (progress - a.position) / span;
            return lerp_color(a.color, b.color, t);
        }
    }
    last.color
}

fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let inv = 1.0 - t;
    Color {
        r: (a.r as f32 * inv + b.r as f32 * t) as u8,
        g: (a.g as f32 * inv + b.g as f32 * t) as u8,
        b: (a.b as f32 * inv + b.b as f32 * t) as u8,
        a: (a.a as f32 * inv + b.a as f32 * t) as u8,
    }
}

fn fill_rounded_rect(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    color: Color,
    rect: Rect,
    radii: CornerRadii,
) {
    // Fall back to the cheaper rectangle filler when no corner is rounded.
    if radii.tl == 0.0 && radii.tr == 0.0 && radii.br == 0.0 && radii.bl == 0.0 {
        fill_rect(buffer, width, height, color, rect);
        return;
    }

    let x_start = rect.x.max(0.0).floor() as usize;
    let y_start = rect.y.max(0.0).floor() as usize;
    let x_end = (rect.x + rect.width).ceil().max(0.0) as usize;
    let y_end = (rect.y + rect.height).ceil().max(0.0) as usize;
    let x_end = x_end.min(width);
    let y_end = y_end.min(height);

    if color.a == 0 {
        return;
    }

    // Cap each radius to half the rect so corners never overlap.
    let max_radius = (rect.width.min(rect.height) / 2.0).max(0.0);
    let tl = radii.tl.clamp(0.0, max_radius);
    let tr = radii.tr.clamp(0.0, max_radius);
    let br = radii.br.clamp(0.0, max_radius);
    let bl = radii.bl.clamp(0.0, max_radius);

    let left = rect.x;
    let top = rect.y;
    let right = rect.x + rect.width;
    let bottom = rect.y + rect.height;

    let opaque = color.a == 255;
    let pixel = rgb_u32(color);
    let a = color.a as u32;
    let inv = 255 - a;
    let cr = color.r as u32;
    let cg = color.g as u32;
    let cb = color.b as u32;

    for y in y_start..y_end {
        let py = y as f32 + 0.5;
        let row = y * width;
        for x in x_start..x_end {
            let px = x as f32 + 0.5;

            // Pixels in the straight band always paint; only corner regions need a distance check.
            let inside = if tl > 0.0 && px < left + tl && py < top + tl {
                let dx = px - (left + tl);
                let dy = py - (top + tl);
                dx * dx + dy * dy <= tl * tl
            } else if tr > 0.0 && px > right - tr && py < top + tr {
                let dx = px - (right - tr);
                let dy = py - (top + tr);
                dx * dx + dy * dy <= tr * tr
            } else if br > 0.0 && px > right - br && py > bottom - br {
                let dx = px - (right - br);
                let dy = py - (bottom - br);
                dx * dx + dy * dy <= br * br
            } else if bl > 0.0 && px < left + bl && py > bottom - bl {
                let dx = px - (left + bl);
                let dy = py - (bottom - bl);
                dx * dx + dy * dy <= bl * bl
            } else {
                true
            };

            if !inside {
                continue;
            }
            if opaque {
                buffer[row + x] = pixel;
            } else {
                let bg = buffer[row + x];
                let r = (a * cr + inv * ((bg >> 16) & 0xFF)) / 255;
                let g = (a * cg + inv * ((bg >> 8) & 0xFF)) / 255;
                let b = (a * cb + inv * (bg & 0xFF)) / 255;
                buffer[row + x] = (r << 16) | (g << 8) | b;
            }
        }
    }
}

fn draw_text(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    text: &TextCommand,
    fonts: &[fontdue::Font],
) {
    if fonts.is_empty() {
        draw_text_bitmap(buffer, width, height, text);
        return;
    }

    let font_size = text.font_size.max(8.0);
    let ascent = fonts[0]
        .horizontal_line_metrics(font_size)
        .map(|m| m.ascent)
        .unwrap_or(font_size * 0.8);
    let mut cursor_x = text.x;

    for ch in text.text.chars() {
        // Find a font that contains this glyph.
        let font_match = fonts
            .iter()
            .find(|f| f.lookup_glyph_index(ch) != 0 || ch == ' ');

        let Some(font) = font_match else {
            // No font has this glyph — use the bitmap fallback for this character.
            draw_bitmap_char(
                buffer,
                width,
                height,
                ch,
                cursor_x,
                text.y,
                text.color,
                text.font_size,
            );
            cursor_x += text.font_size * 0.75;
            continue;
        };

        let (metrics, bitmap) = font.rasterize(ch, font_size);
        let glyph_y = text.y + ascent - metrics.height as f32 - metrics.ymin as f32;

        for row in 0..metrics.height {
            for col in 0..metrics.width {
                let alpha = bitmap[row * metrics.width + col];
                if alpha == 0 {
                    continue;
                }

                let px = (cursor_x + metrics.xmin as f32 + col as f32).round() as i32;
                let py = (glyph_y + row as f32).round() as i32;

                if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
                    continue;
                }

                let idx = py as usize * width + px as usize;
                // Compose glyph coverage with text color's alpha so opacity
                // (or any pre-multiplied alpha on the color) attenuates the
                // visible glyph, not just AA edges.
                let coverage = (alpha as u32 * text.color.a as u32) / 255;
                if coverage == 0 {
                    continue;
                }
                if coverage >= 255 {
                    buffer[idx] = rgb_u32(text.color);
                } else {
                    let bg = buffer[idx];
                    let inv = 255 - coverage;
                    let r = (coverage * text.color.r as u32 + inv * ((bg >> 16) & 0xFF)) / 255;
                    let g = (coverage * text.color.g as u32 + inv * ((bg >> 8) & 0xFF)) / 255;
                    let b = (coverage * text.color.b as u32 + inv * (bg & 0xFF)) / 255;
                    buffer[idx] = (r << 16) | (g << 8) | b;
                }
            }
        }

        cursor_x += metrics.advance_width;
    }
}

fn draw_text_bitmap(buffer: &mut [u32], width: usize, height: usize, text: &TextCommand) {
    let mut cursor_x = text.x;

    for ch in text.text.chars() {
        draw_bitmap_char(
            buffer,
            width,
            height,
            ch,
            cursor_x,
            text.y,
            text.color,
            text.font_size,
        );
        let scale = (text.font_size / 8.0).max(1.0).round();
        cursor_x += if ch == ' ' { 4.0 * scale } else { 6.0 * scale };
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_bitmap_char(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    ch: char,
    x: f32,
    y: f32,
    color: Color,
    font_size: f32,
) {
    let scale = (font_size / 8.0).max(1.0).round() as usize;
    let cursor_x = x.round() as i32;
    let baseline_y = y.round() as i32;

    if ch == ' ' {
        return;
    }

    let glyph = glyph_pattern(ch);
    for (row_index, row) in glyph.iter().enumerate() {
        for (column_index, pixel) in row.chars().enumerate() {
            if pixel == ' ' {
                continue;
            }
            let px = cursor_x + (column_index * scale) as i32;
            let py = baseline_y + (row_index * scale) as i32;
            fill_rect(
                buffer,
                width,
                height,
                color,
                Rect {
                    x: px as f32,
                    y: py as f32,
                    width: scale as f32,
                    height: scale as f32,
                },
            );
        }
    }
}

fn draw_image(buffer: &mut [u32], width: usize, height: usize, image: &ImageCommand) {
    let x_start = image.x.max(0.0).floor() as usize;
    let y_start = image.y.max(0.0).floor() as usize;
    let x_end = (image.x + image.width).ceil().max(0.0) as usize;
    let y_end = (image.y + image.height).ceil().max(0.0) as usize;
    let x_end = x_end.min(width);
    let y_end = y_end.min(height);

    if image.source_width == 0 || image.source_height == 0 {
        return;
    }

    // Images are scaled with nearest-neighbor sampling to keep the implementation small.
    for y in y_start..y_end {
        let source_y = (((y as f32 - image.y) / image.height.max(1.0)) * image.source_height as f32)
            .floor()
            .clamp(0.0, (image.source_height - 1) as f32) as usize;
        let row = y * width;

        for x in x_start..x_end {
            let source_x = (((x as f32 - image.x) / image.width.max(1.0))
                * image.source_width as f32)
                .floor()
                .clamp(0.0, (image.source_width - 1) as f32) as usize;
            let pixel = image.pixels[source_y * image.source_width + source_x];
            buffer[row + x] = pixel;
        }
    }
}

fn rgb_u32(color: Color) -> u32 {
    (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
}

fn glyph_pattern(ch: char) -> [&'static str; 7] {
    match ch.to_ascii_lowercase() {
        '0' => [
            " ### ", "#   #", "#  ##", "# # #", "##  #", "#   #", " ### ",
        ],
        '1' => [
            "  #  ", " ##  ", "# #  ", "  #  ", "  #  ", "  #  ", "#####",
        ],
        '2' => [
            " ### ", "#   #", "    #", "   # ", "  #  ", " #   ", "#####",
        ],
        '3' => [
            " ### ", "#   #", "    #", " ### ", "    #", "#   #", " ### ",
        ],
        '4' => [
            "   # ", "  ## ", " # # ", "#  # ", "#####", "   # ", "   # ",
        ],
        '5' => [
            "#####", "#    ", "#    ", "#### ", "    #", "#   #", " ### ",
        ],
        '6' => [
            " ### ", "#   #", "#    ", "#### ", "#   #", "#   #", " ### ",
        ],
        '7' => [
            "#####", "    #", "   # ", "  #  ", " #   ", " #   ", " #   ",
        ],
        '8' => [
            " ### ", "#   #", "#   #", " ### ", "#   #", "#   #", " ### ",
        ],
        '9' => [
            " ### ", "#   #", "#   #", " ####", "    #", "#   #", " ### ",
        ],
        'a' => [
            " ### ", "#   #", "#   #", "#####", "#   #", "#   #", "#   #",
        ],
        'b' => [
            "#### ", "#   #", "#   #", "#### ", "#   #", "#   #", "#### ",
        ],
        'c' => [
            " ####", "#    ", "#    ", "#    ", "#    ", "#    ", " ####",
        ],
        'd' => [
            "#### ", "#   #", "#   #", "#   #", "#   #", "#   #", "#### ",
        ],
        'e' => [
            "#####", "#    ", "#    ", "#### ", "#    ", "#    ", "#####",
        ],
        'f' => [
            "#####", "#    ", "#    ", "#### ", "#    ", "#    ", "#    ",
        ],
        'g' => [
            " ####", "#    ", "#    ", "#  ##", "#   #", "#   #", " ####",
        ],
        'h' => [
            "#   #", "#   #", "#   #", "#####", "#   #", "#   #", "#   #",
        ],
        'i' => [
            "#####", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ", "#####",
        ],
        'j' => [
            "#####", "   # ", "   # ", "   # ", "   # ", "#  # ", " ##  ",
        ],
        'k' => [
            "#   #", "#  # ", "# #  ", "##   ", "# #  ", "#  # ", "#   #",
        ],
        'l' => [
            "#    ", "#    ", "#    ", "#    ", "#    ", "#    ", "#####",
        ],
        'm' => [
            "#   #", "## ##", "# # #", "#   #", "#   #", "#   #", "#   #",
        ],
        'n' => [
            "#   #", "##  #", "# # #", "#  ##", "#   #", "#   #", "#   #",
        ],
        'o' => [
            " ### ", "#   #", "#   #", "#   #", "#   #", "#   #", " ### ",
        ],
        'p' => [
            "#### ", "#   #", "#   #", "#### ", "#    ", "#    ", "#    ",
        ],
        'q' => [
            " ### ", "#   #", "#   #", "#   #", "# # #", "#  # ", " ## #",
        ],
        'r' => [
            "#### ", "#   #", "#   #", "#### ", "# #  ", "#  # ", "#   #",
        ],
        's' => [
            " ####", "#    ", "#    ", " ### ", "    #", "    #", "#### ",
        ],
        't' => [
            "#####", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ",
        ],
        'u' => [
            "#   #", "#   #", "#   #", "#   #", "#   #", "#   #", " ### ",
        ],
        'v' => [
            "#   #", "#   #", "#   #", "#   #", "#   #", " # # ", "  #  ",
        ],
        'w' => [
            "#   #", "#   #", "#   #", "# # #", "# # #", "## ##", "#   #",
        ],
        'x' => [
            "#   #", "#   #", " # # ", "  #  ", " # # ", "#   #", "#   #",
        ],
        'y' => [
            "#   #", "#   #", " # # ", "  #  ", "  #  ", "  #  ", "  #  ",
        ],
        'z' => [
            "#####", "    #", "   # ", "  #  ", " #   ", "#    ", "#####",
        ],
        '.' => [
            "     ", "     ", "     ", "     ", "     ", " ### ", " ### ",
        ],
        ':' => [
            "     ", " ### ", " ### ", "     ", " ### ", " ### ", "     ",
        ],
        '!' => [
            " ### ", " ### ", " ### ", " ### ", " ### ", "     ", " ### ",
        ],
        '?' => [
            " ### ", "#   #", "    #", "   # ", "  #  ", "     ", "  #  ",
        ],
        '-' => [
            "     ", "     ", "     ", "#####", "     ", "     ", "     ",
        ],
        '<' => [
            "   # ", "  #  ", " #   ", "#    ", " #   ", "  #  ", "   # ",
        ],
        '>' => [
            " #   ", "  #  ", "   # ", "    #", "   # ", "  #  ", " #   ",
        ],
        '/' => [
            "    #", "   # ", "   # ", "  #  ", " #   ", " #   ", "#    ",
        ],
        _ => [
            "#####", "#   #", "   # ", "  #  ", "  #  ", "     ", "  #  ",
        ],
    }
}

#[cfg(test)]
mod tests {
    use crate::{css, html, layout, render, style};

    use super::{
        Color, CornerRadii, DisplayCommand, ImageCommand, TextCommand, rasterize, translate,
    };

    fn display_list(html_source: &str, css_source: &str) -> Vec<DisplayCommand> {
        let node = html::parse(html_source)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let stylesheet = css::parse(css_source).unwrap();
        let styled = style::style_tree(&node, &[stylesheet]);
        let layout = layout::layout_tree(&styled, 400.0);
        render::build_display_list(&layout)
    }

    #[test]
    fn paints_background_rect_from_padding_box() {
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 100px;
                    height: 40px;
                    padding-left: 5px;
                    padding-right: 7px;
                    padding-top: 3px;
                    padding-bottom: 9px;
                    background-color: #336699;
                }
            "#,
        );

        assert_eq!(
            commands,
            vec![DisplayCommand::SolidRect(
                Color {
                    r: 51,
                    g: 102,
                    b: 153,
                    a: 255,
                },
                crate::layout::Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 112.0,
                    height: 52.0,
                }
            )]
        );
    }

    #[test]
    fn paints_text_nodes_with_inherited_style() {
        let commands = display_list(
            r#"<p class="copy">Hello</p>"#,
            r#"
                .copy {
                    color: #0f0;
                    font-size: 18px;
                }
            "#,
        );

        assert_eq!(
            commands,
            vec![DisplayCommand::Text(TextCommand {
                text: "Hello".into(),
                x: 0.0,
                y: 12.0,
                color: Color {
                    r: 0,
                    g: 255,
                    b: 0,
                    a: 255,
                },
                font_size: 18.0,
            })]
        );
    }

    #[test]
    fn paints_rect_before_descendant_text() {
        let commands = display_list(
            r#"<div id="card"><p>Hello</p></div>"#,
            r#"
                #card {
                    background-color: #111111;
                }

                p {
                    font-size: 20px;
                }
            "#,
        );

        assert!(matches!(commands[0], DisplayCommand::SolidRect(_, _)));
        assert!(matches!(commands[1], DisplayCommand::Text(_)));
    }

    #[test]
    fn rasterizes_background_pixels() {
        let pixels = rasterize(
            &[DisplayCommand::SolidRect(
                Color {
                    r: 255,
                    g: 0,
                    b: 0,
                    a: 255,
                },
                crate::layout::Rect {
                    x: 1.0,
                    y: 1.0,
                    width: 2.0,
                    height: 2.0,
                },
            )],
            4,
            4,
            &[],
        );

        assert_eq!(pixels[5], 0xFF0000);
        assert_eq!(pixels[10], 0xFF0000);
        assert_eq!(pixels[0], 0xFFFFFF);
    }

    #[test]
    fn translates_display_commands() {
        let commands = translate(
            vec![
                DisplayCommand::SolidRect(
                    Color::BLACK,
                    crate::layout::Rect {
                        x: 1.0,
                        y: 2.0,
                        width: 3.0,
                        height: 4.0,
                    },
                ),
                DisplayCommand::Text(TextCommand {
                    text: "hello".into(),
                    x: 5.0,
                    y: 6.0,
                    color: Color::BLACK,
                    font_size: 8.0,
                }),
                DisplayCommand::Image(ImageCommand {
                    x: 7.0,
                    y: 8.0,
                    width: 9.0,
                    height: 10.0,
                    source_width: 1,
                    source_height: 1,
                    pixels: vec![0x112233],
                }),
            ],
            10.0,
            20.0,
        );

        assert_eq!(
            commands[0],
            DisplayCommand::SolidRect(
                Color::BLACK,
                crate::layout::Rect {
                    x: 11.0,
                    y: 22.0,
                    width: 3.0,
                    height: 4.0,
                },
            )
        );
        assert_eq!(
            commands[1],
            DisplayCommand::Text(TextCommand {
                text: "hello".into(),
                x: 15.0,
                y: 26.0,
                color: Color::BLACK,
                font_size: 8.0,
            })
        );
        assert_eq!(
            commands[2],
            DisplayCommand::Image(ImageCommand {
                x: 17.0,
                y: 28.0,
                width: 9.0,
                height: 10.0,
                source_width: 1,
                source_height: 1,
                pixels: vec![0x112233],
            })
        );
    }

    #[test]
    fn rasterizes_image_pixels() {
        let pixels = rasterize(
            &[DisplayCommand::Image(ImageCommand {
                x: 0.0,
                y: 0.0,
                width: 2.0,
                height: 2.0,
                source_width: 2,
                source_height: 2,
                pixels: vec![0xFF0000, 0x00FF00, 0x0000FF, 0xFFFFFF],
            })],
            2,
            2,
            &[],
        );

        assert_eq!(pixels, vec![0xFF0000, 0x00FF00, 0x0000FF, 0xFFFFFF]);
    }

    #[test]
    fn paints_borders_when_color_and_width_are_present() {
        let commands = display_list(
            r#"<div class="panel"></div>"#,
            r#"
                .panel {
                    width: 20px;
                    height: 10px;
                    border-left: 2px;
                    border-right: 2px;
                    border-top: 1px;
                    border-bottom: 3px;
                    border-color: #112233;
                }
            "#,
        );

        assert_eq!(
            commands,
            vec![
                DisplayCommand::SolidRect(
                    Color {
                        r: 17,
                        g: 34,
                        b: 51,
                        a: 255,
                    },
                    crate::layout::Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 24.0,
                        height: 1.0,
                    },
                ),
                DisplayCommand::SolidRect(
                    Color {
                        r: 17,
                        g: 34,
                        b: 51,
                        a: 255,
                    },
                    crate::layout::Rect {
                        x: 0.0,
                        y: 11.0,
                        width: 24.0,
                        height: 3.0,
                    },
                ),
                DisplayCommand::SolidRect(
                    Color {
                        r: 17,
                        g: 34,
                        b: 51,
                        a: 255,
                    },
                    crate::layout::Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 2.0,
                        height: 14.0,
                    },
                ),
                DisplayCommand::SolidRect(
                    Color {
                        r: 17,
                        g: 34,
                        b: 51,
                        a: 255,
                    },
                    crate::layout::Rect {
                        x: 22.0,
                        y: 0.0,
                        width: 2.0,
                        height: 14.0,
                    },
                ),
            ]
        );
    }

    #[test]
    fn css_border_radius_emits_rounded_rect_background() {
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 100px;
                    height: 40px;
                    background-color: #336699;
                    border-radius: 8px;
                }
            "#,
        );

        // First command is the background; non-zero border-radius selects RoundedRect.
        match &commands[0] {
            DisplayCommand::RoundedRect(_, _, radii) => {
                assert_eq!(radii.tl, 8.0);
                assert_eq!(radii.tr, 8.0);
                assert_eq!(radii.br, 8.0);
                assert_eq!(radii.bl, 8.0);
            }
            other => panic!("expected RoundedRect background, got {other:?}"),
        }
    }

    #[test]
    fn css_border_radius_four_value_shorthand_assigns_each_corner() {
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 100px;
                    height: 40px;
                    background-color: #336699;
                    border-radius: 1px 2px 3px 4px;
                }
            "#,
        );

        // 4-value shorthand maps to tl/tr/br/bl in source order.
        match &commands[0] {
            DisplayCommand::RoundedRect(_, _, radii) => {
                assert_eq!(radii.tl, 1.0);
                assert_eq!(radii.tr, 2.0);
                assert_eq!(radii.br, 3.0);
                assert_eq!(radii.bl, 4.0);
            }
            other => panic!("expected RoundedRect background, got {other:?}"),
        }
    }

    #[test]
    fn rounded_rect_with_zero_radius_matches_solid_rect() {
        let rect = crate::layout::Rect {
            x: 0.0,
            y: 0.0,
            width: 4.0,
            height: 4.0,
        };
        let color = Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };

        let solid = rasterize(&[DisplayCommand::SolidRect(color, rect)], 4, 4, &[]);
        let rounded = rasterize(
            &[DisplayCommand::RoundedRect(
                color,
                rect,
                CornerRadii::default(),
            )],
            4,
            4,
            &[],
        );

        assert_eq!(solid, rounded);
    }

    #[test]
    fn rounded_rect_uniform_radius_clips_all_four_corners() {
        let pixels = rasterize(
            &[DisplayCommand::RoundedRect(
                Color {
                    r: 255,
                    g: 0,
                    b: 0,
                    a: 255,
                },
                crate::layout::Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 4.0,
                    height: 4.0,
                },
                CornerRadii::uniform(2.0),
            )],
            4,
            4,
            &[],
        );

        // All four corner pixels lie outside the inscribed circle and stay white.
        assert_eq!(pixels[0], 0xFFFFFF, "(0,0) clipped by tl");
        assert_eq!(pixels[3], 0xFFFFFF, "(3,0) clipped by tr");
        assert_eq!(pixels[12], 0xFFFFFF, "(0,3) clipped by bl");
        assert_eq!(pixels[15], 0xFFFFFF, "(3,3) clipped by br");

        // Pixels just inside each corner stay filled.
        assert_eq!(pixels[5], 0xFF0000, "(1,1) inside tl arc");
        assert_eq!(pixels[10], 0xFF0000, "(2,2) inside br arc");
    }

    #[test]
    fn rounded_rect_per_corner_radii_only_clip_specified_corner() {
        let pixels = rasterize(
            &[DisplayCommand::RoundedRect(
                Color {
                    r: 255,
                    g: 0,
                    b: 0,
                    a: 255,
                },
                crate::layout::Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 4.0,
                    height: 4.0,
                },
                CornerRadii {
                    tl: 2.0,
                    tr: 0.0,
                    br: 0.0,
                    bl: 0.0,
                },
            )],
            4,
            4,
            &[],
        );

        // Only the top-left corner is rounded; the other three corners stay sharp.
        assert_eq!(pixels[0], 0xFFFFFF, "(0,0) clipped by tl");
        assert_eq!(pixels[3], 0xFF0000, "(3,0) tr is sharp");
        assert_eq!(pixels[12], 0xFF0000, "(0,3) bl is sharp");
        assert_eq!(pixels[15], 0xFF0000, "(3,3) br is sharp");
    }

    #[test]
    fn translate_moves_rounded_rect_position_only() {
        let commands = translate(
            vec![DisplayCommand::RoundedRect(
                Color::BLACK,
                crate::layout::Rect {
                    x: 1.0,
                    y: 2.0,
                    width: 10.0,
                    height: 6.0,
                },
                CornerRadii::uniform(3.0),
            )],
            4.0,
            5.0,
        );

        assert_eq!(
            commands[0],
            DisplayCommand::RoundedRect(
                Color::BLACK,
                crate::layout::Rect {
                    x: 5.0,
                    y: 7.0,
                    width: 10.0,
                    height: 6.0,
                },
                CornerRadii::uniform(3.0),
            )
        );
    }

    fn solid_rect_colors(commands: &[DisplayCommand]) -> Vec<Color> {
        commands
            .iter()
            .filter_map(|cmd| match cmd {
                DisplayCommand::SolidRect(color, _) => Some(*color),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn positioned_box_paints_after_in_flow_sibling_even_when_earlier_in_dom() {
        // .abs is a position:absolute box that comes BEFORE .flow in DOM order.
        // Without stacking-context handling it would paint first and end up
        // covered by .flow. With the new pass it gets pushed to the positioned
        // layer and paints AFTER .flow.
        let commands = display_list(
            r#"<div id="root"><div class="abs"></div><div class="flow"></div></div>"#,
            r#"
                #root { width: 200px; height: 100px; background-color: #ffffff; }
                .abs {
                    position: absolute;
                    width: 50px;
                    height: 50px;
                    background-color: #ff0000;
                }
                .flow {
                    width: 200px;
                    height: 30px;
                    background-color: #0000ff;
                }
            "#,
        );

        let colors = solid_rect_colors(&commands);
        let red = Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let blue = Color {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        };
        let red_idx = colors.iter().position(|c| *c == red).expect("red painted");
        let blue_idx = colors
            .iter()
            .position(|c| *c == blue)
            .expect("blue painted");

        assert!(
            blue_idx < red_idx,
            "in-flow blue ({blue_idx}) should paint before absolute red ({red_idx})"
        );
    }

    #[test]
    fn z_index_orders_positioned_siblings_ascending() {
        // Two absolutes; the one with z=1 should paint first, z=2 second.
        let commands = display_list(
            r#"<div id="root"><div class="back"></div><div class="front"></div></div>"#,
            r#"
                #root { width: 200px; height: 100px; }
                .back {
                    position: absolute;
                    z-index: 2;
                    width: 50px;
                    height: 50px;
                    background-color: #00ff00;
                }
                .front {
                    position: absolute;
                    z-index: 1;
                    width: 50px;
                    height: 50px;
                    background-color: #ff8800;
                }
            "#,
        );

        let colors = solid_rect_colors(&commands);
        let green = Color {
            r: 0,
            g: 255,
            b: 0,
            a: 255,
        };
        let orange = Color {
            r: 255,
            g: 136,
            b: 0,
            a: 255,
        };
        let green_idx = colors
            .iter()
            .position(|c| *c == green)
            .expect("green painted");
        let orange_idx = colors
            .iter()
            .position(|c| *c == orange)
            .expect("orange painted");

        // z=1 (orange) paints before z=2 (green). DOM order is reversed —
        // proves z-index drives ordering, not source order.
        assert!(
            orange_idx < green_idx,
            "z=1 ({orange_idx}) should paint before z=2 ({green_idx})"
        );
    }

    #[test]
    fn negative_z_index_paints_behind_in_flow_content() {
        // .behind has z=-1, so it sits underneath the in-flow .flow even
        // though the absolute would otherwise paint after.
        let commands = display_list(
            r#"<div id="root"><div class="behind"></div><div class="flow"></div></div>"#,
            r#"
                #root { width: 200px; height: 100px; }
                .behind {
                    position: absolute;
                    z-index: -1;
                    width: 50px;
                    height: 50px;
                    background-color: #ff0000;
                }
                .flow {
                    width: 200px;
                    height: 30px;
                    background-color: #0000ff;
                }
            "#,
        );

        let colors = solid_rect_colors(&commands);
        let red = Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let blue = Color {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        };
        let red_idx = colors.iter().position(|c| *c == red).expect("red painted");
        let blue_idx = colors
            .iter()
            .position(|c| *c == blue)
            .expect("blue painted");

        assert!(
            red_idx < blue_idx,
            "z=-1 red ({red_idx}) should paint before in-flow blue ({blue_idx})"
        );
    }

    #[test]
    fn nested_positioned_descendants_belong_to_their_own_stacking_context() {
        // .outer is absolute. .inner (also absolute) is its child. From the
        // root's perspective .outer is one atomic z-layer — its children
        // should paint within that atom, NOT escape to the root's layer order.
        let commands = display_list(
            r#"<div id="root"><div class="flow"></div><div class="outer"><div class="inner"></div></div></div>"#,
            r#"
                #root { width: 200px; height: 100px; }
                .flow {
                    width: 200px;
                    height: 30px;
                    background-color: #888888;
                }
                .outer {
                    position: absolute;
                    width: 100px;
                    height: 60px;
                    background-color: #ff0000;
                }
                .inner {
                    position: absolute;
                    width: 30px;
                    height: 30px;
                    background-color: #00ff00;
                }
            "#,
        );

        let colors = solid_rect_colors(&commands);
        let gray = Color {
            r: 136,
            g: 136,
            b: 136,
            a: 255,
        };
        let red = Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let green = Color {
            r: 0,
            g: 255,
            b: 0,
            a: 255,
        };
        let gray_idx = colors
            .iter()
            .position(|c| *c == gray)
            .expect("gray painted");
        let red_idx = colors.iter().position(|c| *c == red).expect("red painted");
        let green_idx = colors
            .iter()
            .position(|c| *c == green)
            .expect("green painted");

        // Order: in-flow gray, then absolute outer (red), then nested inner (green).
        assert!(gray_idx < red_idx, "in-flow before outer absolute");
        assert!(
            red_idx < green_idx,
            "outer paints before its inner descendant"
        );
    }

    #[test]
    fn text_glyph_is_offset_by_half_leading_inside_line_box() {
        // 40px line-height with 20px font-size leaves (40-20)/2 = 10px of
        // half-leading above the glyph. The text command y should land at
        // line_box_top + half_leading, which centers the glyph in the line.
        let commands = display_list(
            r#"<p>X</p>"#,
            r#"
                p {
                    font-size: 20px;
                    line-height: 40px;
                    margin-top: 0;
                    margin-bottom: 0;
                }
            "#,
        );

        let text = commands
            .iter()
            .find_map(|cmd| match cmd {
                DisplayCommand::Text(t) => Some(t),
                _ => None,
            })
            .expect("paragraph emits a Text command");

        // p has no margin/padding, so its content origin is (0, 0). The line
        // box top is at content_y = 0; glyph sits 10px below that.
        assert_eq!(text.y, 10.0);
        // font-size in the command stays 20 — line-height does not scale
        // glyph rendering, only the surrounding box.
        assert_eq!(text.font_size, 20.0);
    }

    fn first_solid_alpha_for(commands: &[DisplayCommand]) -> u8 {
        commands
            .iter()
            .find_map(|cmd| match cmd {
                DisplayCommand::SolidRect(color, _) => Some(color.a),
                _ => None,
            })
            .expect("at least one SolidRect")
    }

    #[test]
    fn opacity_attenuates_emitted_color_alpha() {
        // `opacity: 0.5` should multiply the background color's alpha by 0.5
        // when the SolidRect is emitted — 255 * 0.5 = 127.
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 50px;
                    height: 50px;
                    background-color: #ff0000;
                    opacity: 0.5;
                }
            "#,
        );

        assert_eq!(first_solid_alpha_for(&commands), 127);
    }

    #[test]
    fn nested_opacities_multiply() {
        // Inner `.b`'s alpha = parent 0.5 × own 0.5 = 0.25 → 255 × 0.25 = 63.
        let commands = display_list(
            r#"<div class="a"><div class="b"></div></div>"#,
            r#"
                .a {
                    width: 100px;
                    height: 100px;
                    background-color: #ff0000;
                    opacity: 0.5;
                }
                .b {
                    width: 50px;
                    height: 50px;
                    background-color: #0000ff;
                    opacity: 0.5;
                }
            "#,
        );

        let alphas: Vec<u8> = commands
            .iter()
            .filter_map(|cmd| match cmd {
                DisplayCommand::SolidRect(color, _) => Some(color.a),
                _ => None,
            })
            .collect();
        // Two rects: parent first (.a, alpha = 127), then child (.b, alpha = 63).
        assert_eq!(alphas[0], 127);
        assert_eq!(alphas[1], 63);
    }

    #[test]
    fn opacity_inherits_through_non_positioned_ancestor_into_positioned_descendant() {
        // The hard case: positioned descendants jump out of the normal paint
        // walk into a z-layer, so the alpha they inherit must come from the
        // collected ancestor chain — not from the stacking context root.
        // Final alpha for `.c` = 0.5 × 0.5 × 0.5 = 0.125 → 255 × 0.125 = 31.
        let commands = display_list(
            r#"<div class="a"><div class="b"><div class="c"></div></div></div>"#,
            r#"
                .a { width: 200px; height: 200px; opacity: 0.5; }
                .b { width: 100px; height: 100px; opacity: 0.5; }
                .c {
                    position: absolute;
                    width: 50px;
                    height: 50px;
                    background-color: #ff0000;
                    opacity: 0.5;
                }
            "#,
        );

        // Find the red rect — that's `.c`'s background.
        let red_alpha = commands
            .iter()
            .find_map(|cmd| match cmd {
                DisplayCommand::SolidRect(color, _) if color.r == 255 && color.g == 0 => {
                    Some(color.a)
                }
                _ => None,
            })
            .expect("red rect for .c");
        assert_eq!(red_alpha, 31);
    }

    #[test]
    fn linear_gradient_vertical_red_to_blue_interpolates_top_to_bottom() {
        // 1×4 strip with `linear-gradient(red, blue)` — top row should be
        // mostly red, bottom row mostly blue. Exact midpoints depend on
        // pixel-center sampling, so we just check the dominant channel.
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 1px;
                    height: 4px;
                    background-image: linear-gradient(red, blue);
                }
            "#,
        );
        let pixels = render::rasterize(&commands, 1, 4, &[]);

        let top_r = (pixels[0] >> 16) & 0xFF;
        let top_b = pixels[0] & 0xFF;
        let bottom_r = (pixels[3] >> 16) & 0xFF;
        let bottom_b = pixels[3] & 0xFF;

        assert!(top_r > 200, "top should be mostly red, got r={top_r}");
        assert!(top_b < 50, "top should have little blue, got b={top_b}");
        assert!(
            bottom_r < 50,
            "bottom should have little red, got r={bottom_r}"
        );
        assert!(
            bottom_b > 200,
            "bottom should be mostly blue, got b={bottom_b}"
        );
    }

    #[test]
    fn linear_gradient_to_right_interpolates_left_to_right() {
        // Same gradient, rotated to the horizontal axis — direction wins.
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 4px;
                    height: 1px;
                    background-image: linear-gradient(to right, red, blue);
                }
            "#,
        );
        let pixels = render::rasterize(&commands, 4, 1, &[]);

        let left_r = (pixels[0] >> 16) & 0xFF;
        let left_b = pixels[0] & 0xFF;
        let right_r = (pixels[3] >> 16) & 0xFF;
        let right_b = pixels[3] & 0xFF;

        assert!(left_r > 200, "left should be mostly red, got r={left_r}");
        assert!(
            right_b > 200,
            "right should be mostly blue, got b={right_b}"
        );
        assert!(left_b < 50);
        assert!(right_r < 50);
    }

    #[test]
    fn linear_gradient_explicit_stop_positions_pin_color_at_those_points() {
        // With `red 0%, blue 25%, blue 100%`, every pixel from x=1 onward in
        // a 4px wide row should already be pure blue (the second stop pins it).
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 4px;
                    height: 1px;
                    background-image: linear-gradient(to right, red 0%, blue 25%, blue 100%);
                }
            "#,
        );
        let pixels = render::rasterize(&commands, 4, 1, &[]);

        // Pixel index 1 sits at progress = 1.5/4 = 0.375 ≥ 0.25 → fully blue.
        assert_eq!(pixels[1], 0x000000FF);
        assert_eq!(pixels[2], 0x000000FF);
        assert_eq!(pixels[3], 0x000000FF);
    }

    #[test]
    fn text_shadow_emits_offset_text_command_before_main_text() {
        // `text-shadow: 2px 3px red` should produce two Text commands: the
        // shadow at (offset_x, offset_y) under the main glyph in red, and
        // the regular text on top with the inherited color.
        let commands = display_list(
            r#"<p>Hi</p>"#,
            r#"
                p {
                    font-size: 16px;
                    color: black;
                    text-shadow: 2px 3px red;
                    margin-top: 0;
                    margin-bottom: 0;
                }
            "#,
        );

        let texts: Vec<&TextCommand> = commands
            .iter()
            .filter_map(|cmd| match cmd {
                DisplayCommand::Text(text) => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(texts.len(), 2, "shadow + main = 2 text commands");

        let shadow = texts[0];
        let main = texts[1];

        // Shadow color = red.
        assert_eq!(shadow.color.r, 255);
        assert_eq!(shadow.color.g, 0);
        assert_eq!(shadow.color.b, 0);
        // Main color = black (inherited).
        assert_eq!(main.color.r, 0);
        assert_eq!(main.color.g, 0);
        assert_eq!(main.color.b, 0);

        // Shadow sits at +2,+3 relative to the main text.
        assert!((shadow.x - main.x - 2.0).abs() < f32::EPSILON);
        assert!((shadow.y - main.y - 3.0).abs() < f32::EPSILON);
        // Same glyph string and size.
        assert_eq!(shadow.text, main.text);
        assert_eq!(shadow.font_size, main.font_size);
    }

    #[test]
    fn text_without_text_shadow_emits_only_one_text_command() {
        // Sanity check: the shadow command shouldn't sneak in when no
        // text-shadow is declared on the element or any ancestor.
        let commands = display_list(
            r#"<p>Hi</p>"#,
            r#"
                p { font-size: 16px; }
            "#,
        );
        let text_count = commands
            .iter()
            .filter(|cmd| matches!(cmd, DisplayCommand::Text(_)))
            .count();
        assert_eq!(text_count, 1);
    }

    #[test]
    fn box_shadow_offset_paints_solid_outside_box_with_no_blur() {
        // 2×2 box at (0, 0) with `box-shadow: 2px 2px 0 0 red`. Shadow lands
        // at (2, 2)–(4, 4) with no blur, so pixels there should be solid red
        // and pixels inside the box itself stay covered by its own bg.
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 2px;
                    height: 2px;
                    background-color: white;
                    box-shadow: 2px 2px 0 0 red;
                }
            "#,
        );
        let pixels = render::rasterize(&commands, 4, 4, &[]);

        // (3, 3): inside the shadow, fully red.
        assert_eq!(pixels[3 * 4 + 3], 0x00FF0000);
        // (0, 0): inside the box's own white background — shadow not visible there.
        assert_eq!(pixels[0], 0x00FFFFFF);
    }

    #[test]
    fn box_shadow_blur_softens_alpha_outside_rect() {
        // `box-shadow: 0 0 4px black` with no offset. Inside the rect the
        // box's own white bg paints over the shadow, so the test focuses on
        // pixels OUTSIDE: ones close to the edge get a soft darken from the
        // linear-ramp blur, ones beyond the blur radius are untouched.
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 4px;
                    height: 4px;
                    background-color: white;
                    box-shadow: 0 0 4px black;
                }
            "#,
        );
        // 12×12 buffer leaves at least a 4px margin on every side of the box.
        let pixels = render::rasterize(&commands, 12, 12, &[]);

        // (5, 2): 1.5px past the right edge → coverage 1 - 1.5/4 = 0.625,
        // pixel reads roughly mid-gray.
        let near = pixels[2 * 12 + 5];
        let near_r = (near >> 16) & 0xFF;
        assert!(
            near_r < 200,
            "near-edge pixel should be visibly darkened, got r={near_r}"
        );

        // (8, 2): 4.5px past the right edge — beyond the blur radius, so
        // coverage clamps to 0 and the pixel stays full white.
        let far = pixels[2 * 12 + 8];
        assert_eq!(
            far, 0x00FFFFFF,
            "pixel beyond the blur radius should be untouched"
        );
    }

    #[test]
    fn radial_gradient_centers_inner_color_with_outer_at_corners() {
        // 5×5 box with `radial-gradient(red, blue)` (ellipse, farthest-corner).
        // Center pixel should be the inner stop (red); corner pixels should
        // sample close to the outer stop (blue) since their normalised
        // distance from the centre approaches 1.
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 5px;
                    height: 5px;
                    background-image: radial-gradient(red, blue);
                }
            "#,
        );
        let pixels = render::rasterize(&commands, 5, 5, &[]);

        let center = pixels[2 * 5 + 2];
        let center_r = (center >> 16) & 0xFF;
        let center_b = center & 0xFF;
        assert!(
            center_r > 200,
            "center should be near red, got r={center_r}"
        );
        assert!(
            center_b < 50,
            "center should have little blue, got b={center_b}"
        );

        // Top-left corner: distance ≈ sqrt(2)/2·diag → progress ≈ 1 → blue.
        let corner = pixels[0];
        let corner_r = (corner >> 16) & 0xFF;
        let corner_b = corner & 0xFF;
        assert!(
            corner_b > 200,
            "corner should be near blue, got b={corner_b}"
        );
        assert!(
            corner_r < 50,
            "corner should have little red, got r={corner_r}"
        );
    }

    #[test]
    fn fill_rect_alpha_blends_with_existing_pixel() {
        // White buffer + 50% red → red channel stays 255, green/blue mix to ~127.
        let red_half = Color {
            r: 255,
            g: 0,
            b: 0,
            a: 128,
        };
        let pixels = render::rasterize(
            &[DisplayCommand::SolidRect(
                red_half,
                crate::layout::Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
            )],
            1,
            1,
            &[],
        );

        // Expected: r ≈ 255, g and b ≈ 127. Single u32 = 0xFF7F7F.
        assert_eq!(pixels[0], 0x00FF7F7F);
    }

    #[test]
    fn transform_translate_shifts_emitted_solid_rect() {
        // `transform: translate(5px, 10px)` should leave the box's logical
        // size alone but move the painted rect by (5, 10).
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 20px;
                    height: 8px;
                    background-color: red;
                    transform: translate(5px, 10px);
                }
            "#,
        );

        let rect = match commands.as_slice() {
            [DisplayCommand::SolidRect(_, rect)] => *rect,
            other => panic!("expected one SolidRect, got {other:?}"),
        };
        // Logical box was at (0, 0) with width 20, height 8. The translate
        // shifts the origin only — width and height stay invariant for now.
        assert_eq!(rect.x, 5.0);
        assert_eq!(rect.y, 10.0);
        assert_eq!(rect.width, 20.0);
        assert_eq!(rect.height, 8.0);
    }

    #[test]
    fn transform_translate_inherits_to_descendant() {
        // The parent's translate should compose into the child's emitted
        // commands as well (paint-pass thread of `inherited_transform`).
        let commands = display_list(
            r#"<div id="outer"><div id="inner"></div></div>"#,
            r#"
                #outer { transform: translate(50px, 0); }
                #inner {
                    width: 10px;
                    height: 4px;
                    background-color: blue;
                }
            "#,
        );

        let inner_rect = commands
            .iter()
            .find_map(|cmd| match cmd {
                DisplayCommand::SolidRect(_, rect) if rect.width == 10.0 => Some(*rect),
                _ => None,
            })
            .expect("inner rect must be emitted");
        assert_eq!(inner_rect.x, 50.0);
        assert_eq!(inner_rect.y, 0.0);
    }

    #[test]
    fn affine_inverse_undoes_translate() {
        // Round-trip: applying a translate then its inverse to a point must
        // return the original point. This is the operation hit-test relies on.
        let t = super::Affine::translate(7.0, -3.5);
        let (x, y) = t.compose(t.inverse()).apply_point(11.0, 22.0);
        assert!((x - 11.0).abs() < 1e-5);
        assert!((y - 22.0).abs() < 1e-5);

        // The inverse of a translate is just the negation of the offsets.
        let inv = t.inverse();
        let (x, y) = inv.apply_point(10.0, 10.0);
        assert!((x - 3.0).abs() < 1e-5);
        assert!((y - 13.5).abs() < 1e-5);
    }
}
