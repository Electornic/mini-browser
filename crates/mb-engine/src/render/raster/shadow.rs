// `box-shadow` rasteriser. The linear falloff outside the rect has no
// clean tiny-skia shader equivalent, so we drive a per-pixel scan
// against `Pixmap::data_mut()`. Two paths:
//   - `fill_box_shadow_aligned` walks axis-aligned shadows in screen
//     space directly (the common case)
//   - `paint_through` (in sibling `blend`) handles the rotated case by
//     walking the AABB of the rotated quad and sampling coverage
//     through the inverse transform

use tiny_skia::Pixmap;

use crate::css::Color;
use crate::layout::Rect;

use super::super::{Affine, ShadowCommand};
use super::blend::{blend_pixel_bytes, paint_through};

pub(super) fn fill_box_shadow(pixmap: &mut Pixmap, shadow: &ShadowCommand, transform: Affine) {
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
