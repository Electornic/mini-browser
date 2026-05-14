// DisplayCommand -> pixel buffer. Software paint pipeline backed by
// `tiny-skia`: a `Pixmap` of premultiplied RGBA bytes is the buffer.
// Most primitives go through tiny-skia natively:
//   - SolidRect          fill_rect with a solid Paint
//   - RoundedRect        fill_path on a cubic-bezier corner approximation
//   - Image              fill_rect with a Pattern shader (nearest filter)
//   - Gradient           fill_rect with LinearGradient / RadialGradient
//                        shaders; radial uses gradient_transform =
//                        scale(rx, ry).post_translate(cx, cy) so a
//                        non-square box keeps elliptical falloff
//
// Two helpers stay as custom byte loops driven against `pixmap.data_mut()`:
//   - BoxShadow          linear-ramp coverage outside the rect — no
//                        clean tiny-skia equivalent for this falloff
//   - Text glyphs        cosmic-text shapes runs, swash rasterises each
//                        glyph; we blit the mask / color image into the
//                        pixmap directly. Both have a slow path through
//                        `paint_through` for rotation.
//
// Anti-aliasing is forced off on every Paint (tiny-skia's `Paint::default()`
// has it on): the test surface compares exact pixel u32s against geometric
// shapes whose boundary rule is "pixel centre ≤ radius / inside the rect",
// and AA edge coverage flips those boundary pixels.

mod shapes;

use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Metrics, PhysicalGlyph, Shaping, SwashContent, Weight,
};
use tiny_skia::{
    Color as TsColor, FilterQuality, GradientStop, LinearGradient, Paint, Pattern, Pixmap,
    Point as TsPoint, RadialGradient, Rect as TsRect, SpreadMode, Transform as TsTransform,
};

use crate::css::{Color, GradientDirection, GradientKind};
use crate::layout::Rect;

use shapes::{affine_to_ts, fill_rounded_rect, fill_solid_rect};

use super::{Affine, DisplayCommand, GradientCommand, ImageCommand, ShadowCommand, TextCommand};

// Measures the rendered width of `text` at `font_size`. Callers use this to
// position UI elements that need to align with the *end* of a rendered string
// (caret, link underlines, line packing) without a fixed average glyph width
// — which is always wrong for proportional fonts.
//
// When the shared cosmic-text `FontSystem` is installed, the result reflects
// real shaping (kerning, ligatures, font fallback). Tests that never call
// `state::install_fonts` see the deterministic toy estimate so layout
// assertions don't depend on the host's font set.
pub fn measure_text_width(text: &str, font_size: f32) -> f32 {
    measure_text_wrap(text, font_size, None).0
}

/// Translate the cascaded `font-family` + `font-weight` into the
/// cosmic-text `Attrs` the shaper consults. Only the generic
/// `monospace` family keyword is acted on today; weight is plumbed as
/// a numeric value so 700+ picks the bold face from the matched
/// family. Kept private because the only sensible callers are the
/// measure / shape paths that already exist in this file.
fn attrs_for_run(family_keyword: Option<&str>, font_weight: u16) -> Attrs<'static> {
    let mut attrs = Attrs::new();
    if let Some("monospace") = family_keyword {
        attrs = attrs.family(Family::Monospace);
    }
    // Weight defaults to 400 in the StyledNode helper; only emit a Weight
    // override when it diverges, so the cosmic-text default font face
    // continues to resolve through the same path it always did for
    // non-bold runs.
    if font_weight != 400 {
        attrs = attrs.weight(Weight(font_weight));
    }
    attrs
}

// Measures `text` shaped by cosmic-text, breaking the run at `wrap_width`
// when it is `Some` so the result reflects how a paragraph would wrap inside
// a content box of that width. Returns `(max_line_width, line_count)` —
// inline layout uses both: max_line_width for line packing, line_count for
// the vertical space the wrapped paragraph reserves.
//
// When no `FontSystem` is installed (every unit test that does not call
// `state::install_fonts`), the toy `font_size * 0.75` per-char estimate
// runs and the caller always sees a single line.
pub fn measure_text_wrap(text: &str, font_size: f32, wrap_width: Option<f32>) -> (f32, u32) {
    measure_text_wrap_with_family(text, font_size, wrap_width, None, 400)
}

/// Family + weight-aware variant of [`measure_text_wrap`]. The
/// `family_keyword` is the lowercased cascaded `font-family` value (see
/// `display_list::font_family_keyword`); `font_weight` is the resolved
/// CSS numeric scale (1-1000). Inline layout uses this so a `<b>` /
/// `<th>` run measures with the same bold face the paint pass will use,
/// instead of computing shrink-to-fit widths with the regular face and
/// then drawing wider bold glyphs.
pub fn measure_text_wrap_with_family(
    text: &str,
    font_size: f32,
    wrap_width: Option<f32>,
    family_keyword: Option<&str>,
    font_weight: u16,
) -> (f32, u32) {
    if let Some(metrics) =
        measure_with_cosmic(text, font_size, wrap_width, family_keyword, font_weight)
    {
        return metrics;
    }
    let scale = (font_size / 8.0).max(1.0).round();
    let width: f32 = text
        .chars()
        .map(|ch| if ch == ' ' { 4.0 * scale } else { 6.0 * scale })
        .sum();
    (width, 1)
}

fn measure_with_cosmic(
    text: &str,
    font_size: f32,
    wrap_width: Option<f32>,
    family_keyword: Option<&str>,
    font_weight: u16,
) -> Option<(f32, u32)> {
    let slot = crate::font_system::shared_font_system()?;
    let mut fs = slot.lock().ok()?;

    let size = font_size.max(8.0);
    // Line height does not affect line_w, but cosmic-text requires a nonzero value.
    let metrics = Metrics::new(size, size * 1.2);
    let mut buffer = Buffer::new(&mut fs, metrics);
    // `borrow_with` ties the buffer to the font system so subsequent calls
    // can be written without re-passing the font system on every line.
    let mut buffer = buffer.borrow_with(&mut fs);
    // `wrap_width = None` shapes the whole string as one unwrapped line; a
    // `Some(w)` constraint asks cosmic-text to find break opportunities so
    // the longest visual line fits inside `w`.
    buffer.set_size(wrap_width, None);
    let attrs = attrs_for_run(family_keyword, font_weight);
    buffer.set_text(text, &attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(true);

    let mut width = 0.0_f32;
    let mut lines: u32 = 0;
    for run in buffer.layout_runs() {
        if run.line_w > width {
            width = run.line_w;
        }
        lines += 1;
    }
    Some((width, lines.max(1)))
}

pub fn rasterize(commands: &[DisplayCommand], width: usize, height: usize) -> Vec<u32> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let mut pixmap = Pixmap::new(width as u32, height as u32)
        .expect("non-zero width/height should fit a Pixmap allocation");
    pixmap.fill(TsColor::WHITE);

    for command in commands {
        rasterize_command(&mut pixmap, command, Affine::IDENTITY);
    }

    pixmap_to_u32(&pixmap)
}

// The pixmap's alpha is always 255 (we start with opaque white and every
// blend equation is source-over, which preserves dst.a == 255), so
// premultiplied RGB bytes equal straight RGB bytes — drop the alpha and
// pack as 0x00RRGGBB to match what the legacy `Vec<u32>` buffer held.
fn pixmap_to_u32(pixmap: &Pixmap) -> Vec<u32> {
    let data = pixmap.data();
    let mut out = Vec::with_capacity(data.len() / 4);
    for chunk in data.chunks_exact(4) {
        out.push((u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]));
    }
    out
}

fn rasterize_command(pixmap: &mut Pixmap, command: &DisplayCommand, transform: Affine) {
    match command {
        DisplayCommand::SolidRect(color, rect) => {
            fill_solid_rect(pixmap, *color, *rect, transform)
        }
        DisplayCommand::RoundedRect(color, rect, radii) => {
            fill_rounded_rect(pixmap, *color, *rect, *radii, transform)
        }
        DisplayCommand::Text(text) => draw_text(pixmap, text, transform),
        DisplayCommand::Image(image) => draw_image(pixmap, image, transform),
        DisplayCommand::Gradient(gradient) => fill_gradient(pixmap, gradient, transform),
        DisplayCommand::BoxShadow(shadow) => fill_box_shadow(pixmap, shadow, transform),
        DisplayCommand::TransformGroup(group_xform, inner) => {
            // Compose this group's matrix on top of any inherited transform.
            // The display-list builder only emits rotation/shear here (axis-
            // aligned matrices are baked into rect coords up there), so we
            // expect `transform` to be identity in practice — but composing
            // is the right semantics if it ever isn't.
            let composed = transform.compose(*group_xform);
            for cmd in inner {
                rasterize_command(pixmap, cmd, composed);
            }
        }
    }
}

fn fill_gradient(pixmap: &mut Pixmap, gradient: &GradientCommand, transform: Affine) {
    if gradient.stops.len() < 2 {
        return;
    }
    let rect = gradient.rect;
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }
    let Some(dest) = TsRect::from_xywh(rect.x, rect.y, rect.width, rect.height) else {
        return;
    };

    let stops: Vec<GradientStop> = gradient
        .stops
        .iter()
        .map(|s| {
            GradientStop::new(
                s.position,
                TsColor::from_rgba8(s.color.r, s.color.g, s.color.b, s.color.a),
            )
        })
        .collect();

    let cx = rect.x + rect.width * 0.5;
    let cy = rect.y + rect.height * 0.5;
    let rx = rect.width * 0.5;
    let ry = rect.height * 0.5;

    let shader = match gradient.kind {
        GradientKind::Linear(direction) => {
            // Pin the gradient axis to whichever box dimension the
            // direction runs along; the cross axis stays at the box centre
            // so the colour bands are perpendicular to the axis.
            let (start, end) = match direction {
                GradientDirection::ToBottom => (
                    TsPoint::from_xy(cx, rect.y),
                    TsPoint::from_xy(cx, rect.y + rect.height),
                ),
                GradientDirection::ToTop => (
                    TsPoint::from_xy(cx, rect.y + rect.height),
                    TsPoint::from_xy(cx, rect.y),
                ),
                GradientDirection::ToRight => (
                    TsPoint::from_xy(rect.x, cy),
                    TsPoint::from_xy(rect.x + rect.width, cy),
                ),
                GradientDirection::ToLeft => (
                    TsPoint::from_xy(rect.x + rect.width, cy),
                    TsPoint::from_xy(rect.x, cy),
                ),
            };
            LinearGradient::new(start, end, stops, SpreadMode::Pad, TsTransform::identity())
        }
        GradientKind::Radial => {
            // Bake the box's aspect ratio into the gradient's transform: a
            // unit-circle radial at local origin, scaled by (rx, ry) and
            // translated to the box centre, becomes an ellipse aligned to
            // the rect. Matches CSS `radial-gradient(...)` with the default
            // farthest-corner ellipse extent.
            let gradient_transform = TsTransform::from_scale(rx, ry).post_translate(cx, cy);
            RadialGradient::new(
                TsPoint::from_xy(0.0, 0.0),
                0.0,
                TsPoint::from_xy(0.0, 0.0),
                1.0,
                stops,
                SpreadMode::Pad,
                gradient_transform,
            )
        }
    };
    let Some(shader) = shader else {
        return;
    };

    let paint = Paint {
        anti_alias: false,
        shader,
        ..Default::default()
    };
    pixmap.fill_rect(dest, &paint, affine_to_ts(transform), None);
}

fn fill_box_shadow(pixmap: &mut Pixmap, shadow: &ShadowCommand, transform: Affine) {
    if shadow.color.a == 0 {
        return;
    }
    if shadow.rect.width <= 0.0 || shadow.rect.height <= 0.0 {
        return;
    }
    let blur = shadow.blur_radius.max(0.0);

    if transform.is_identity() {
        fill_box_shadow_aligned(pixmap, shadow, blur);
    } else {
        // The blur falloff lives outside the rect, so the logical bounds
        // expand by `blur` on every side before we walk screen-space.
        let bounds = Rect {
            x: shadow.rect.x - blur,
            y: shadow.rect.y - blur,
            width: shadow.rect.width + 2.0 * blur,
            height: shadow.rect.height + 2.0 * blur,
        };
        let color = shadow.color;
        let rect = shadow.rect;
        let inverse = transform.inverse();
        paint_through(pixmap, bounds, transform, inverse, |lx, ly| {
            let coverage = shadow_coverage(lx, ly, rect, blur);
            if coverage <= 0.0 {
                return None;
            }
            let alpha = ((color.a as f32) * coverage).clamp(0.0, 255.0) as u8;
            Some(Color {
                r: color.r,
                g: color.g,
                b: color.b,
                a: alpha,
            })
        });
    }
}

fn fill_box_shadow_aligned(pixmap: &mut Pixmap, shadow: &ShadowCommand, blur: f32) {
    let (width, height) = (pixmap.width() as usize, pixmap.height() as usize);
    // Affected region = shadow rect inflated by `blur` on every side. Anything
    // farther than `blur` from the rect edge has zero coverage.
    let x_start = (shadow.rect.x - blur).max(0.0).floor() as usize;
    let y_start = (shadow.rect.y - blur).max(0.0).floor() as usize;
    let x_end =
        (((shadow.rect.x + shadow.rect.width + blur).ceil()).max(0.0) as usize).min(width);
    let y_end =
        (((shadow.rect.y + shadow.rect.height + blur).ceil()).max(0.0) as usize).min(height);

    let left = shadow.rect.x;
    let top = shadow.rect.y;
    let right = shadow.rect.x + shadow.rect.width;
    let bottom = shadow.rect.y + shadow.rect.height;

    let data = pixmap.data_mut();
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
            let combined_alpha = ((shadow.color.a as f32) * coverage) as u8;
            if combined_alpha == 0 {
                continue;
            }
            let blended = Color {
                r: shadow.color.r,
                g: shadow.color.g,
                b: shadow.color.b,
                a: combined_alpha,
            };
            blend_pixel_bytes(data, (y * width + x) * 4, blended);
        }
    }
}

fn draw_image(pixmap: &mut Pixmap, image: &ImageCommand, transform: Affine) {
    if image.source_width == 0 || image.source_height == 0 {
        return;
    }
    if image.width <= 0.0 || image.height <= 0.0 {
        return;
    }
    if image.pixels.len() < image.source_width * image.source_height {
        return;
    }

    // Stage the source pixels into a tiny-skia tile pixmap once, then
    // rely on the Pattern shader to handle scaling + rotation natively.
    // Source pixels arrive as 0xRRGGBB (alpha-stripped); bake them into
    // RGBA bytes with alpha=255 — the test surface treats every loaded
    // image as fully opaque.
    let sw = image.source_width as u32;
    let sh = image.source_height as u32;
    let Some(mut tile) = Pixmap::new(sw, sh) else {
        return;
    };
    {
        let dst = tile.data_mut();
        for (i, &pixel) in image.pixels.iter().take(image.source_width * image.source_height).enumerate() {
            let off = i * 4;
            dst[off] = ((pixel >> 16) & 0xFF) as u8;
            dst[off + 1] = ((pixel >> 8) & 0xFF) as u8;
            dst[off + 2] = (pixel & 0xFF) as u8;
            dst[off + 3] = 255;
        }
    }

    // Pattern transform places the tile on the canvas. There are two
    // shapes we need to render:
    //
    // 1. `<img>` and un-positioned `background-image: url(...)` —
    //    stretch the source pixels to fill the box (scale_x/y derived
    //    from the dest:source ratio).
    // 2. Sprite slice via `background-position: -Npx -Mpx` — render at
    //    the source's native pixel size, with the source origin shifted
    //    by (source_x, source_y) so the box reveals only that slice.
    //
    // Phase 6.G's branching: when either source_x or source_y is
    // non-zero we know the page asked for the sprite shape; otherwise
    // the legacy stretch path runs and existing `<img>` / bg tests
    // continue to assert the same geometry.
    let is_sprite = image.source_x != 0.0 || image.source_y != 0.0;
    let pattern_transform = if is_sprite {
        TsTransform::from_translate(image.x - image.source_x, image.y - image.source_y)
    } else {
        let scale_x = image.width / image.source_width as f32;
        let scale_y = image.height / image.source_height as f32;
        TsTransform::from_scale(scale_x, scale_y).post_translate(image.x, image.y)
    };

    let paint = Paint {
        anti_alias: false,
        shader: Pattern::new(
            tile.as_ref(),
            SpreadMode::Pad,
            FilterQuality::Nearest,
            1.0,
            pattern_transform,
        ),
        ..Default::default()
    };

    let Some(dest) = TsRect::from_xywh(image.x, image.y, image.width, image.height) else {
        return;
    };
    pixmap.fill_rect(dest, &paint, affine_to_ts(transform), None);
}

fn paint_through<F>(
    pixmap: &mut Pixmap,
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
    let (width, height) = (pixmap.width() as usize, pixmap.height() as usize);
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

    let data = pixmap.data_mut();
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
            blend_pixel_bytes(data, (row + x) * 4, color);
        }
    }
}

// Source-over blend a single pixel at byte offset `byte_idx` (premultiplied
// RGBA layout). Fast paths the opaque case and assumes the destination
// alpha is already 255 — true throughout this rasterizer because we start
// with an opaque white pixmap and never use a non source-over blend mode.
fn blend_pixel_bytes(data: &mut [u8], byte_idx: usize, color: Color) {
    if color.a == 255 {
        data[byte_idx] = color.r;
        data[byte_idx + 1] = color.g;
        data[byte_idx + 2] = color.b;
        data[byte_idx + 3] = 255;
        return;
    }
    let a = color.a as u32;
    let inv = 255 - a;
    let bg_r = data[byte_idx] as u32;
    let bg_g = data[byte_idx + 1] as u32;
    let bg_b = data[byte_idx + 2] as u32;
    let r = (a * color.r as u32 + inv * bg_r) / 255;
    let g = (a * color.g as u32 + inv * bg_g) / 255;
    let b = (a * color.b as u32 + inv * bg_b) / 255;
    data[byte_idx] = r as u8;
    data[byte_idx + 1] = g as u8;
    data[byte_idx + 2] = b as u8;
    data[byte_idx + 3] = 255;
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

fn draw_text(pixmap: &mut Pixmap, text: &TextCommand, transform: Affine) {
    if transform.is_identity() {
        draw_text_aligned(pixmap, text);
    } else {
        draw_text_through(pixmap, text, transform);
    }
}

fn draw_text_aligned(pixmap: &mut Pixmap, text: &TextCommand) {
    // When no shared FontSystem is installed (tests, or before
    // `state::install_fonts` has run), `shape_and_images` returns `None` and
    // we hand off to the 7x7 bitmap fallback so the test surface stays
    // deterministic without loading any host font.
    let Some(physicals_and_images) = shape_and_images(text) else {
        draw_text_bitmap(pixmap, text);
        return;
    };
    for (physical, image) in &physicals_and_images {
        match image.content {
            SwashContent::Mask => {
                blit_swash_mask(pixmap, image, physical, text.color);
            }
            SwashContent::Color => {
                blit_swash_color(pixmap, image, physical);
            }
            // Subpixel masks would need a different per-channel coverage
            // blend; cosmic-text's default Metrics shaping pipeline does
            // not emit them so this branch is currently unreachable.
            SwashContent::SubpixelMask => {}
        }
    }
}

fn draw_text_through(pixmap: &mut Pixmap, text: &TextCommand, transform: Affine) {
    // Per-glyph: get the swash alpha image (laid out in its own local
    // coordinates), then for every pixel in the screen-space bbox of the
    // glyph quad, inverse-map back to glyph-local and sample the bitmap.
    // The glyph itself never needs to know about rotation — only the
    // placement does.
    //
    // No bitmap fallback under transform: the 7x7 toy font has no notion of
    // rotation, so when no shared FontSystem is installed (test paths) we
    // simply skip the run rather than paint glyphs at the wrong orientation.
    let Some(physicals_and_images) = shape_and_images(text) else {
        return;
    };
    let inverse = transform.inverse();
    for (physical, image) in &physicals_and_images {
        // Color glyphs (emoji) under transform are uncommon and would need a
        // bespoke premultiplied source-over inverse-mapped blend; defer them
        // by skipping rather than painting wrong colours.
        if !matches!(image.content, SwashContent::Mask) {
            continue;
        }
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
        paint_through(pixmap, glyph_bounds, transform, inverse, |lx, ly| {
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
        });
    }
}

// Shape `text.text` through cosmic-text and resolve every glyph to its swash
// image, returning `(physical_glyph, image)` pairs ready to blit. Both the
// FontSystem and SwashCache live in shared `Mutex` slots — we acquire each
// lock for the smallest possible scope. Returning `None` signals the caller
// to fall back to the bitmap path (e.g. the shared slots have not been
// initialised yet, or the lock is poisoned). Empty placements (whitespace,
// non-printing glyphs) are dropped here so the paint loop only iterates real
// pixel-bearing glyphs.
fn shape_and_images(text: &TextCommand) -> Option<Vec<(PhysicalGlyph, cosmic_text::SwashImage)>> {
    let fs_slot = crate::font_system::shared_font_system()?;
    let swash_slot = crate::font_system::shared_swash_cache()?;
    let mut fs = fs_slot.lock().ok()?;
    let mut swash = swash_slot.lock().ok()?;

    let physicals = shape_to_physicals(&mut fs, text);
    let mut out = Vec::with_capacity(physicals.len());
    for physical in physicals {
        if let Some(image) = swash.get_image(&mut fs, physical.cache_key).clone()
            && image.placement.width != 0
            && image.placement.height != 0
        {
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
    // `wrap_width` mirrors the layout-time wrap decision so the rendered
    // glyphs land on the same lines layout reserved space for. `None` means
    // the caller wants the whole string as one unwrapped line (chrome,
    // single-line input values).
    bw.set_size(text.wrap_width, None);
    let attrs = attrs_for_run(text.font_family.as_deref(), text.font_weight);
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
    pixmap: &mut Pixmap,
    image: &cosmic_text::SwashImage,
    physical: &PhysicalGlyph,
    color: Color,
) {
    let (width, height) = (pixmap.width() as i32, pixmap.height() as i32);
    let img_w = image.placement.width as usize;
    let img_h = image.placement.height as usize;
    // `placement.left` is the bearing from the glyph origin to the image's
    // left edge; `placement.top` is bearing UP from the baseline to the
    // image's top edge (so we subtract to get screen y).
    let dx0 = physical.x + image.placement.left;
    let dy0 = physical.y - image.placement.top;

    let stride = pixmap.width() as usize;
    let data = pixmap.data_mut();
    for row in 0..img_h {
        for col in 0..img_w {
            let alpha = image.data[row * img_w + col];
            if alpha == 0 {
                continue;
            }
            let px = dx0 + col as i32;
            let py = dy0 + row as i32;
            if px < 0 || py < 0 || px >= width || py >= height {
                continue;
            }
            // Compose glyph coverage with text color's alpha so opacity (or
            // any pre-multiplied alpha on the color) attenuates the visible
            // glyph, not just AA edges.
            let coverage = (alpha as u32 * color.a as u32) / 255;
            if coverage == 0 {
                continue;
            }
            let blended = Color {
                r: color.r,
                g: color.g,
                b: color.b,
                a: coverage as u8,
            };
            blend_pixel_bytes(data, (py as usize * stride + px as usize) * 4, blended);
        }
    }
}

// Paint a swash color glyph (e.g. emoji from CBDT/sbix/COLR-CPAL tables).
// Pixels arrive as 32-bit BGRA with **premultiplied alpha**, so the source-over
// equation is `out = src + bg * (255 - a) / 255` — no further multiplication
// of the source channels is needed. The TextCommand's `color` is intentionally
// ignored: a colored glyph carries its own pixel colors, and tinting it would
// drain the chroma that makes the emoji recognisable.
fn blit_swash_color(
    pixmap: &mut Pixmap,
    image: &cosmic_text::SwashImage,
    physical: &PhysicalGlyph,
) {
    let (width, height) = (pixmap.width() as i32, pixmap.height() as i32);
    let img_w = image.placement.width as usize;
    let img_h = image.placement.height as usize;
    let dx0 = physical.x + image.placement.left;
    let dy0 = physical.y - image.placement.top;

    let stride = pixmap.width() as usize;
    let data = pixmap.data_mut();
    for row in 0..img_h {
        for col in 0..img_w {
            let i = (row * img_w + col) * 4;
            let b = image.data[i] as u32;
            let g = image.data[i + 1] as u32;
            let r = image.data[i + 2] as u32;
            let a = image.data[i + 3] as u32;
            if a == 0 {
                continue;
            }
            let px = dx0 + col as i32;
            let py = dy0 + row as i32;
            if px < 0 || py < 0 || px >= width || py >= height {
                continue;
            }
            let off = (py as usize * stride + px as usize) * 4;
            if a == 255 {
                data[off] = r as u8;
                data[off + 1] = g as u8;
                data[off + 2] = b as u8;
                data[off + 3] = 255;
            } else {
                let inv = 255 - a;
                let bg_r = data[off] as u32;
                let bg_g = data[off + 1] as u32;
                let bg_b = data[off + 2] as u32;
                let out_r = (r + bg_r * inv / 255).min(255);
                let out_g = (g + bg_g * inv / 255).min(255);
                let out_b = (b + bg_b * inv / 255).min(255);
                data[off] = out_r as u8;
                data[off + 1] = out_g as u8;
                data[off + 2] = out_b as u8;
                data[off + 3] = 255;
            }
        }
    }
}

fn draw_text_bitmap(pixmap: &mut Pixmap, text: &TextCommand) {
    let mut cursor_x = text.x;

    for ch in text.text.chars() {
        draw_bitmap_char(pixmap, ch, cursor_x, text.y, text.color, text.font_size);
        let scale = (text.font_size / 8.0).max(1.0).round();
        cursor_x += if ch == ' ' { 4.0 * scale } else { 6.0 * scale };
    }
}

fn draw_bitmap_char(
    pixmap: &mut Pixmap,
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
            fill_solid_rect(
                pixmap,
                color,
                Rect {
                    x: px as f32,
                    y: py as f32,
                    width: scale as f32,
                    height: scale as f32,
                },
                Affine::IDENTITY,
            );
        }
    }
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
