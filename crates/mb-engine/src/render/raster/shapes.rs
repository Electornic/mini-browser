// Rect / rounded-rect rasterisers plus the two tiny tiny-skia conversion
// helpers (`to_ts_rect`, `affine_to_ts`) every other rasteriser in this
// directory uses. Anti-aliasing is forced off on every paint here so the
// pixel-pinned tests at the 4×4 / 5×5 surface scales stay deterministic.

use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect as TsRect, Transform as TsTransform};

use crate::css::Color;
use crate::layout::Rect;

use super::super::{Affine, CornerRadii};

pub(super) fn fill_solid_rect(pixmap: &mut Pixmap, color: Color, rect: Rect, transform: Affine) {
    if color.a == 0 {
        return;
    }
    let Some(ts_rect) = to_ts_rect(rect) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    // tiny-skia's `Paint::default()` has `anti_alias = true`. Force it off
    // so a rotated rect's edges stay jagged the way the old hand-rolled
    // inverse-pixel-sample produced — what the pixel-pinned tests assert.
    paint.anti_alias = false;
    pixmap.fill_rect(ts_rect, &paint, affine_to_ts(transform), None);
}

// Quarter-circle cubic bezier coefficient: distance from the on-curve
// endpoint to its off-curve control point, expressed as a fraction of the
// radius. Skia / SVG / Inkscape all use 4*(sqrt(2)-1)/3 — the curve
// deviates from a true circle by under 0.03% so pixel-pinned tests at our
// 4×4 / 5×5 surface scales never see the difference.
const QUARTER_ARC_K: f32 = 0.5522848;

pub(super) fn fill_rounded_rect(
    pixmap: &mut Pixmap,
    color: Color,
    rect: Rect,
    radii: CornerRadii,
    transform: Affine,
) {
    if color.a == 0 {
        return;
    }
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }
    if radii.tl == 0.0 && radii.tr == 0.0 && radii.br == 0.0 && radii.bl == 0.0 {
        // Pure rectangle: skip the path build and reuse the fast fill_rect path.
        fill_solid_rect(pixmap, color, rect, transform);
        return;
    }

    // Cap each radius to half the rect so adjacent corners never overlap.
    let max_radius = (rect.width.min(rect.height) / 2.0).max(0.0);
    let tl = radii.tl.clamp(0.0, max_radius);
    let tr = radii.tr.clamp(0.0, max_radius);
    let br = radii.br.clamp(0.0, max_radius);
    let bl = radii.bl.clamp(0.0, max_radius);

    let l = rect.x;
    let t = rect.y;
    let r = rect.x + rect.width;
    let b = rect.y + rect.height;

    let mut pb = PathBuilder::new();
    pb.move_to(l + tl, t);
    pb.line_to(r - tr, t);
    if tr > 0.0 {
        let k = tr * QUARTER_ARC_K;
        pb.cubic_to(r - tr + k, t, r, t + tr - k, r, t + tr);
    }
    pb.line_to(r, b - br);
    if br > 0.0 {
        let k = br * QUARTER_ARC_K;
        pb.cubic_to(r, b - br + k, r - br + k, b, r - br, b);
    }
    pb.line_to(l + bl, b);
    if bl > 0.0 {
        let k = bl * QUARTER_ARC_K;
        pb.cubic_to(l + bl - k, b, l, b - bl + k, l, b - bl);
    }
    pb.line_to(l, t + tl);
    if tl > 0.0 {
        let k = tl * QUARTER_ARC_K;
        pb.cubic_to(l, t + tl - k, l + tl - k, t, l + tl, t);
    }
    pb.close();
    let Some(path) = pb.finish() else {
        return;
    };

    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = false;
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        affine_to_ts(transform),
        None,
    );
}

pub(super) fn to_ts_rect(rect: Rect) -> Option<TsRect> {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return None;
    }
    TsRect::from_xywh(rect.x, rect.y, rect.width, rect.height)
}

// Our `Affine` stores the matrix as
//     | a c e |
//     | b d f |
//     | 0 0 1 |
// while tiny-skia's `from_row(sx, ky, kx, sy, tx, ty)` expects
//     | sx kx tx |
//     | ky sy ty |
//     | 0  0  1  |
// so the direct mapping is sx=a, ky=b, kx=c, sy=d, tx=e, ty=f.
pub(super) fn affine_to_ts(a: Affine) -> TsTransform {
    TsTransform::from_row(a.a, a.b, a.c, a.d, a.e, a.f)
}
