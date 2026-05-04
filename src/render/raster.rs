// DisplayCommand -> pixel buffer. Software rasterizer for solid rects,
// rounded rects, gradients, box-shadows, text (cosmic-text shaping +
// swash glyph rasterisation, with a 7x7 bitmap font fallback), and images.
// Translate+scale matrices are baked into rect coordinates by the
// display-list stage; rotation lands here as a `TransformGroup` and is
// scan-converted through the inverse matrix per-pixel.

use cosmic_text::{Attrs, Buffer, FontSystem, Metrics, PhysicalGlyph, Shaping, SwashContent};

use crate::css::{Color, GradientDirection, GradientKind};
use crate::layout::Rect;

use super::{
    Affine, CornerRadii, DisplayCommand, GradientCommand, ImageCommand, ResolvedStop,
    ShadowCommand, TextCommand,
};

// Glyph image caching is now owned by `cosmic_text::SwashCache`, which keys
// rasterised images on a `CacheKey` that already encodes (font id, glyph id,
// size, subpixel position). The cache lives as a `OnceLock<Mutex<SwashCache>>`
// in `state` and is rebuilt whenever the FontSystem swaps after navigation,
// so this stub stays around purely so `main.rs`'s explicit invalidation marker
// does not have to be plumbed through a removed API.
pub fn invalidate_glyph_cache() {}

// Measures the rendered width of `text` at `font_size`. Callers use this to
// position UI elements that need to align with the *end* of a rendered string
// (caret, link underlines, line packing) without a fixed average glyph width
// — which is always wrong for proportional fonts.
//
// Phase 4.4a: when the shared cosmic-text `FontSystem` is initialised, we route
// through a Buffer so the result reflects real shaping (kerning, ligatures,
// font fallback). The legacy fontdue per-char advance path is kept as a
// fallback for the brief window before `build_font_cache` has run, and the
// empty-`fonts`-slice path keeps unit tests deterministic without loading any
// real font.
pub fn measure_text_width(text: &str, font_size: f32, fonts: &[fontdue::Font]) -> f32 {
    if fonts.is_empty() {
        let scale = (font_size / 8.0).max(1.0).round();
        return text
            .chars()
            .map(|ch| if ch == ' ' { 4.0 * scale } else { 6.0 * scale })
            .sum();
    }

    if let Some(width) = measure_with_cosmic(text, font_size) {
        return width;
    }

    measure_with_fontdue(text, font_size, fonts)
}

fn measure_with_cosmic(text: &str, font_size: f32) -> Option<f32> {
    let slot = crate::state::shared_font_system()?;
    let mut fs = slot.lock().ok()?;

    let size = font_size.max(8.0);
    // Line height does not affect line_w, but cosmic-text requires a nonzero value.
    let metrics = Metrics::new(size, size * 1.2);
    let mut buffer = Buffer::new(&mut fs, metrics);
    // `borrow_with` ties the buffer to the font system so subsequent calls
    // can be written without re-passing the font system on every line.
    let mut buffer = buffer.borrow_with(&mut fs);
    // Unconstrained width so the input is never wrapped — we want the full
    // shaped advance of `text`, not the width of the longest line after wrap.
    buffer.set_size(None, None);
    let attrs = Attrs::new();
    buffer.set_text(text, &attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(true);

    let mut width = 0.0_f32;
    for run in buffer.layout_runs() {
        if run.line_w > width {
            width = run.line_w;
        }
    }
    Some(width)
}

fn measure_with_fontdue(text: &str, font_size: f32, fonts: &[fontdue::Font]) -> f32 {
    let size = font_size.max(8.0);
    let mut width = 0.0_f32;
    for ch in text.chars() {
        let font_match = fonts
            .iter()
            .find(|f| f.lookup_glyph_index(ch) != 0 || ch == ' ');
        match font_match {
            Some(font) => {
                // `font.metrics(ch, size)` returns advance + bounding box
                // *without* rasterising the glyph bitmap; using `rasterize`
                // here was making the per-frame cost scale with page text
                // length on HN-sized pages.
                let metrics = font.metrics(ch, size);
                width += metrics.advance_width;
            }
            None => width += font_size * 0.75,
        }
    }
    width
}

pub fn rasterize(
    commands: &[DisplayCommand],
    width: usize,
    height: usize,
    fonts: &[fontdue::Font],
) -> Vec<u32> {
    let mut buffer = vec![rgb_u32(Color::WHITE); width * height];

    for command in commands {
        rasterize_command(&mut buffer, width, height, command, fonts);
    }

    buffer
}

fn rasterize_command(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    command: &DisplayCommand,
    fonts: &[fontdue::Font],
) {
    match command {
        DisplayCommand::SolidRect(color, rect) => fill_rect(buffer, width, height, *color, *rect),
        DisplayCommand::RoundedRect(color, rect, radii) => {
            fill_rounded_rect(buffer, width, height, *color, *rect, *radii)
        }
        DisplayCommand::Text(text) => draw_text(buffer, width, height, text, fonts),
        DisplayCommand::Image(image) => draw_image(buffer, width, height, image),
        DisplayCommand::Gradient(gradient) => fill_gradient(buffer, width, height, gradient),
        DisplayCommand::BoxShadow(shadow) => fill_box_shadow(buffer, width, height, shadow),
        DisplayCommand::TransformGroup(transform, inner) => {
            rasterize_through_transform(buffer, width, height, *transform, inner, fonts);
        }
    }
}

fn rasterize_through_transform(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    transform: Affine,
    commands: &[DisplayCommand],
    fonts: &[fontdue::Font],
) {
    // Slow path: each inner primitive's logical bounds are mapped to a
    // screen-space bounding box through `transform`; every pixel in that
    // bbox is inverse-mapped back to logical space and sampled against
    // the primitive there. The matrix never has to be axis-aligned, so
    // rotation and shear come out correct.
    let inverse = transform.inverse();
    for command in commands {
        match command {
            DisplayCommand::SolidRect(color, rect) => {
                let color = *color;
                let rect = *rect;
                paint_through(buffer, width, height, rect, transform, inverse, |lx, ly| {
                    if point_in_logical_rect(lx, ly, rect) {
                        Some(color)
                    } else {
                        None
                    }
                });
            }
            DisplayCommand::RoundedRect(color, rect, radii) => {
                let color = *color;
                let rect = *rect;
                let radii = *radii;
                paint_through(buffer, width, height, rect, transform, inverse, |lx, ly| {
                    if point_in_logical_rounded_rect(lx, ly, rect, radii) {
                        Some(color)
                    } else {
                        None
                    }
                });
            }
            DisplayCommand::Gradient(gradient) => {
                let rect = gradient.rect;
                paint_through(buffer, width, height, rect, transform, inverse, |lx, ly| {
                    let progress = gradient_progress(lx, ly, rect, gradient.kind);
                    let color = sample_gradient(&gradient.stops, progress.clamp(0.0, 1.0));
                    if color.a == 0 { None } else { Some(color) }
                });
            }
            DisplayCommand::BoxShadow(shadow) => {
                // Logical bounds expand by blur on every side because the
                // soft falloff lives outside the rect itself.
                let blur = shadow.blur_radius.max(0.0);
                let bounds = Rect {
                    x: shadow.rect.x - blur,
                    y: shadow.rect.y - blur,
                    width: shadow.rect.width + 2.0 * blur,
                    height: shadow.rect.height + 2.0 * blur,
                };
                paint_through(
                    buffer,
                    width,
                    height,
                    bounds,
                    transform,
                    inverse,
                    |lx, ly| {
                        let coverage = shadow_coverage(lx, ly, shadow.rect, blur);
                        if coverage <= 0.0 {
                            return None;
                        }
                        let alpha = ((shadow.color.a as f32) * coverage).clamp(0.0, 255.0) as u8;
                        Some(Color {
                            r: shadow.color.r,
                            g: shadow.color.g,
                            b: shadow.color.b,
                            a: alpha,
                        })
                    },
                );
            }
            DisplayCommand::Image(image) => {
                let bounds = Rect {
                    x: image.x,
                    y: image.y,
                    width: image.width,
                    height: image.height,
                };
                let sw = image.source_width;
                let sh = image.source_height;
                if sw == 0 || sh == 0 || bounds.width <= 0.0 || bounds.height <= 0.0 {
                    continue;
                }
                paint_through(
                    buffer,
                    width,
                    height,
                    bounds,
                    transform,
                    inverse,
                    |lx, ly| {
                        let u = (lx - bounds.x) / bounds.width;
                        let v = (ly - bounds.y) / bounds.height;
                        if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                            return None;
                        }
                        let sx = (u * sw as f32).floor().clamp(0.0, (sw - 1) as f32) as usize;
                        let sy = (v * sh as f32).floor().clamp(0.0, (sh - 1) as f32) as usize;
                        let pixel = image.pixels[sy * sw + sx];
                        Some(Color {
                            r: ((pixel >> 16) & 0xFF) as u8,
                            g: ((pixel >> 8) & 0xFF) as u8,
                            b: (pixel & 0xFF) as u8,
                            a: 255,
                        })
                    },
                );
            }
            DisplayCommand::Text(text) => {
                draw_text_through(buffer, width, height, text, transform, inverse, fonts);
            }
            DisplayCommand::TransformGroup(_, _) => {
                // Nested groups never get emitted by the paint pass — each
                // box wraps its own primitives at most once with the
                // cumulative matrix. Ignore defensively if it ever happens.
            }
        }
    }
}

fn paint_through<F>(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    logical_bounds: Rect,
    transform: Affine,
    inverse: Affine,
    sample: F,
) where
    F: Fn(f32, f32) -> Option<Color>,
{
    if logical_bounds.width <= 0.0 || logical_bounds.height <= 0.0 {
        return;
    }
    // Project the four logical corners through the matrix to find the
    // screen-space rectangle that needs to be scanned. For axis-aligned
    // transforms the four corners collapse onto the original rect; for
    // rotation they describe a rotated quad whose AABB is what we walk.
    let corners = [
        transform.apply_point(logical_bounds.x, logical_bounds.y),
        transform.apply_point(logical_bounds.x + logical_bounds.width, logical_bounds.y),
        transform.apply_point(
            logical_bounds.x + logical_bounds.width,
            logical_bounds.y + logical_bounds.height,
        ),
        transform.apply_point(logical_bounds.x, logical_bounds.y + logical_bounds.height),
    ];
    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for (cx, cy) in corners {
        min_x = min_x.min(cx);
        max_x = max_x.max(cx);
        min_y = min_y.min(cy);
        max_y = max_y.max(cy);
    }
    let x_start = min_x.max(0.0).floor() as usize;
    let y_start = min_y.max(0.0).floor() as usize;
    let x_end = (max_x.ceil().max(0.0) as usize).min(width);
    let y_end = (max_y.ceil().max(0.0) as usize).min(height);

    for y in y_start..y_end {
        let row = y * width;
        for x in x_start..x_end {
            let (lx, ly) = inverse.apply_point(x as f32 + 0.5, y as f32 + 0.5);
            let Some(color) = sample(lx, ly) else {
                continue;
            };
            if color.a == 0 {
                continue;
            }
            blend_pixel(&mut buffer[row + x], color);
        }
    }
}

fn blend_pixel(slot: &mut u32, color: Color) {
    if color.a == 255 {
        *slot = rgb_u32(color);
        return;
    }
    let bg = *slot;
    let a = color.a as u32;
    let inv = 255 - a;
    let r = (a * color.r as u32 + inv * ((bg >> 16) & 0xFF)) / 255;
    let g = (a * color.g as u32 + inv * ((bg >> 8) & 0xFF)) / 255;
    let b = (a * color.b as u32 + inv * (bg & 0xFF)) / 255;
    *slot = (r << 16) | (g << 8) | b;
}

fn point_in_logical_rect(lx: f32, ly: f32, rect: Rect) -> bool {
    lx >= rect.x && lx < rect.x + rect.width && ly >= rect.y && ly < rect.y + rect.height
}

fn point_in_logical_rounded_rect(lx: f32, ly: f32, rect: Rect, radii: CornerRadii) -> bool {
    if !point_in_logical_rect(lx, ly, rect) {
        return false;
    }
    let max_r = (rect.width.min(rect.height) / 2.0).max(0.0);
    let tl = radii.tl.clamp(0.0, max_r);
    let tr = radii.tr.clamp(0.0, max_r);
    let br = radii.br.clamp(0.0, max_r);
    let bl = radii.bl.clamp(0.0, max_r);
    let left = rect.x;
    let top = rect.y;
    let right = rect.x + rect.width;
    let bottom = rect.y + rect.height;
    if tl > 0.0 && lx < left + tl && ly < top + tl {
        let dx = lx - (left + tl);
        let dy = ly - (top + tl);
        return dx * dx + dy * dy <= tl * tl;
    }
    if tr > 0.0 && lx > right - tr && ly < top + tr {
        let dx = lx - (right - tr);
        let dy = ly - (top + tr);
        return dx * dx + dy * dy <= tr * tr;
    }
    if br > 0.0 && lx > right - br && ly > bottom - br {
        let dx = lx - (right - br);
        let dy = ly - (bottom - br);
        return dx * dx + dy * dy <= br * br;
    }
    if bl > 0.0 && lx < left + bl && ly > bottom - bl {
        let dx = lx - (left + bl);
        let dy = ly - (bottom - bl);
        return dx * dx + dy * dy <= bl * bl;
    }
    true
}

fn gradient_progress(lx: f32, ly: f32, rect: Rect, kind: GradientKind) -> f32 {
    match kind {
        GradientKind::Linear(GradientDirection::ToBottom) => (ly - rect.y) / rect.height,
        GradientKind::Linear(GradientDirection::ToTop) => 1.0 - (ly - rect.y) / rect.height,
        GradientKind::Linear(GradientDirection::ToRight) => (lx - rect.x) / rect.width,
        GradientKind::Linear(GradientDirection::ToLeft) => 1.0 - (lx - rect.x) / rect.width,
        GradientKind::Radial => {
            let cx = rect.x + rect.width * 0.5;
            let cy = rect.y + rect.height * 0.5;
            let nx = (lx - cx) / (rect.width * 0.5);
            let ny = (ly - cy) / (rect.height * 0.5);
            (nx * nx + ny * ny).sqrt()
        }
    }
}

fn shadow_coverage(lx: f32, ly: f32, rect: Rect, blur: f32) -> f32 {
    // Same linear falloff fill_box_shadow uses for the fast path: distance
    // to the nearest point inside the rect, normalised by the blur radius.
    let left = rect.x;
    let top = rect.y;
    let right = rect.x + rect.width;
    let bottom = rect.y + rect.height;
    let dx = (left - lx).max(0.0).max(lx - right);
    let dy = (top - ly).max(0.0).max(ly - bottom);
    let dist = (dx * dx + dy * dy).sqrt();
    if blur > 0.0 {
        (1.0 - dist / blur).clamp(0.0, 1.0)
    } else if dist == 0.0 {
        1.0
    } else {
        0.0
    }
}

fn draw_text_through(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    text: &TextCommand,
    transform: Affine,
    inverse: Affine,
    fonts: &[fontdue::Font],
) {
    // Per-glyph: get the swash alpha image (laid out in its own local
    // coordinates), then for every pixel in the screen-space bbox of the
    // glyph quad, inverse-map back to glyph-local and sample the bitmap.
    // The glyph itself never needs to know about rotation — only the
    // placement does.
    //
    // The bitmap fallback path doesn't support arbitrary transforms, so the
    // empty-fonts branch (tests / no fonts loaded) skips drawing rather than
    // emitting glyphs at the wrong orientation.
    if fonts.is_empty() {
        return;
    }
    let Some(physicals_and_images) = shape_and_images(text) else {
        return;
    };
    for (physical, image) in &physicals_and_images {
        let img_w = image.placement.width as usize;
        let img_h = image.placement.height as usize;
        let dx0 = (physical.x + image.placement.left) as f32;
        let dy0 = (physical.y - image.placement.top) as f32;
        let glyph_bounds = Rect {
            x: dx0,
            y: dy0,
            width: img_w as f32,
            height: img_h as f32,
        };
        let color = text.color;
        let data = &image.data;
        paint_through(
            buffer,
            width,
            height,
            glyph_bounds,
            transform,
            inverse,
            |lx, ly| {
                let gx = (lx - dx0).floor() as i32;
                let gy = (ly - dy0).floor() as i32;
                if gx < 0 || gy < 0 || gx as usize >= img_w || gy as usize >= img_h {
                    return None;
                }
                let alpha = data[gy as usize * img_w + gx as usize];
                if alpha == 0 {
                    return None;
                }
                let coverage = (alpha as u32 * color.a as u32) / 255;
                if coverage == 0 {
                    return None;
                }
                Some(Color {
                    r: color.r,
                    g: color.g,
                    b: color.b,
                    a: coverage as u8,
                })
            },
        );
    }
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
    // Empty fonts means tests are driving the renderer with no real font data
    // installed; the 7x7 bitmap fallback gives them deterministic glyphs that
    // do not depend on cosmic-text or any system font.
    if fonts.is_empty() {
        draw_text_bitmap(buffer, width, height, text);
        return;
    }
    let Some(physicals_and_images) = shape_and_images(text) else {
        draw_text_bitmap(buffer, width, height, text);
        return;
    };
    for (physical, image) in &physicals_and_images {
        blit_swash_mask(buffer, width, height, image, physical, text.color);
    }
}

// Shape `text.text` through cosmic-text and resolve every glyph to its swash
// alpha image, returning `(physical_glyph, image)` pairs ready to blit. Both
// the FontSystem and SwashCache live in shared `Mutex` slots — we acquire
// each lock for the smallest possible scope. Returning `None` signals the
// caller to fall back to the bitmap path (e.g. the shared slots have not been
// initialised yet, or the lock is poisoned).
fn shape_and_images(text: &TextCommand) -> Option<Vec<(PhysicalGlyph, cosmic_text::SwashImage)>> {
    let fs_slot = crate::state::shared_font_system()?;
    let swash_slot = crate::state::shared_swash_cache()?;
    let mut fs = fs_slot.lock().ok()?;
    let mut swash = swash_slot.lock().ok()?;

    let physicals = shape_to_physicals(&mut fs, text);
    let mut out = Vec::with_capacity(physicals.len());
    for physical in physicals {
        if let Some(image) = swash.get_image(&mut fs, physical.cache_key).clone() {
            // Only mask images participate in the alpha-blend path; subpixel
            // and color images would need their own blend (Phase 4.4 follow-up).
            if image.placement.width == 0
                || image.placement.height == 0
                || !matches!(image.content, SwashContent::Mask)
            {
                continue;
            }
            out.push((physical, image));
        }
    }
    Some(out)
}

fn shape_to_physicals(fs: &mut FontSystem, text: &TextCommand) -> Vec<PhysicalGlyph> {
    let size = text.font_size.max(8.0);
    let metrics = Metrics::new(size, size * 1.2);
    let mut buffer = Buffer::new(fs, metrics);
    let mut bw = buffer.borrow_with(fs);
    bw.set_size(None, None);
    let attrs = Attrs::new();
    bw.set_text(&text.text, &attrs, Shaping::Advanced, None);
    bw.shape_until_scroll(true);

    let mut out = Vec::new();
    for run in bw.layout_runs() {
        // `run.line_y` is the baseline of this line in buffer-local
        // coordinates. Offsetting by `(text.x, text.y + run.line_y)` maps
        // the run to absolute screen coordinates with the baseline aligned
        // where `draw_text_bitmap` would have placed it.
        let baseline = text.y + run.line_y;
        for glyph in run.glyphs.iter() {
            out.push(glyph.physical((text.x, baseline), 1.0));
        }
    }
    out
}

fn blit_swash_mask(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    image: &cosmic_text::SwashImage,
    physical: &PhysicalGlyph,
    color: Color,
) {
    let img_w = image.placement.width as usize;
    let img_h = image.placement.height as usize;
    // `placement.left` is the bearing from the glyph origin to the image's
    // left edge; `placement.top` is bearing UP from the baseline to the
    // image's top edge (so we subtract to get screen y).
    let dx0 = physical.x + image.placement.left;
    let dy0 = physical.y - image.placement.top;

    for row in 0..img_h {
        for col in 0..img_w {
            let alpha = image.data[row * img_w + col];
            if alpha == 0 {
                continue;
            }
            let px = dx0 + col as i32;
            let py = dy0 + row as i32;
            if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
                continue;
            }
            let idx = py as usize * width + px as usize;
            // Compose glyph coverage with text color's alpha so opacity (or
            // any pre-multiplied alpha on the color) attenuates the visible
            // glyph, not just AA edges.
            let coverage = (alpha as u32 * color.a as u32) / 255;
            if coverage == 0 {
                continue;
            }
            if coverage >= 255 {
                buffer[idx] = rgb_u32(color);
            } else {
                let bg = buffer[idx];
                let inv = 255 - coverage;
                let r = (coverage * color.r as u32 + inv * ((bg >> 16) & 0xFF)) / 255;
                let g = (coverage * color.g as u32 + inv * ((bg >> 8) & 0xFF)) / 255;
                let b = (coverage * color.b as u32 + inv * (bg & 0xFF)) / 255;
                buffer[idx] = (r << 16) | (g << 8) | b;
            }
        }
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
