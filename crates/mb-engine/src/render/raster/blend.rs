// Per-pixel paint helpers shared by the rotation-fallback paths (the
// `paint_through` scanner) and the text glyph blit (`blend_pixel_bytes`).
// Both write directly into the premultiplied RGBA bytes the rest of the
// rasteriser passes around as `Pixmap::data_mut()` — bypassing tiny-skia
// because tiny-skia has no clean equivalent for "compute per-pixel
// coverage from a closure".

use tiny_skia::Pixmap;

use crate::css::Color;
use crate::layout::Rect;

use super::super::Affine;

/// Walk the screen-space bounding box of `logical_bounds` under
/// `transform`, sample the caller's closure in logical coordinates via
/// `inverse`, and blend the returned color into the pixmap. Used by the
/// rotated-shadow and rotated-text paths — anything tiny-skia can't
/// express as a shader / pattern.
pub(super) fn paint_through<F>(
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
pub(super) fn blend_pixel_bytes(data: &mut [u8], byte_idx: usize, color: Color) {
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
