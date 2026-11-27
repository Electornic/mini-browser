// Rect / rounded-rect rasterisers plus the two tiny tiny-skia conversion
// helpers (`to_ts_rect`, `affine_to_ts`) every other rasteriser in this
// directory uses. Anti-aliasing is forced off on every paint here so the
// pixel-pinned tests at the 4×4 / 5×5 surface scales stay deterministic.

use tiny_skia::{
    FillRule, Mask, Paint, PathBuilder, Pixmap, Rect as TsRect, Transform as TsTransform,
};

use crate::css::Color;
use crate::layout::Rect;

use super::super::{Affine, CornerRadii, RoundedBorderCommand};

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
    let mut pb = PathBuilder::new();
    push_rounded_rect(&mut pb, rect, capped_radii(rect, radii));
    let Some(path) = pb.finish() else {
        return;
    };
    pixmap.fill_path(
        &path,
        &solid_paint(color),
        FillRule::Winding,
        affine_to_ts(transform),
        None,
    );
}

fn solid_paint(color: Color) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = false;
    paint
}

/// Each radius capped to half the rect so adjacent corners never overlap.
fn capped_radii(rect: Rect, radii: CornerRadii) -> CornerRadii {
    let max_radius = (rect.width.min(rect.height) / 2.0).max(0.0);
    let cap = |r: f32| {
        if r.is_finite() {
            r.clamp(0.0, max_radius)
        } else {
            0.0
        }
    };
    CornerRadii {
        tl: cap(radii.tl),
        tr: cap(radii.tr),
        br: cap(radii.br),
        bl: cap(radii.bl),
    }
}

/// One closed rounded-rect contour (clockwise) on `pb`; radii already capped.
fn push_rounded_rect(pb: &mut PathBuilder, rect: Rect, radii: CornerRadii) {
    let CornerRadii { tl, tr, br, bl } = radii;
    let l = rect.x;
    let t = rect.y;
    let r = rect.x + rect.width;
    let b = rect.y + rect.height;
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
}

/// A rounded border ring (follow-up F3): the rounded border box minus the
/// padding box inset by the side widths (even-odd fill). The inner corner
/// radius is the outer one less the wider adjacent side (circular — the
/// spec's inner corner is elliptical when the two widths differ). Sides of
/// different colours each fill the ring through a mask of their trapezoid
/// (outer corner to inner corner), as browsers split corners.
pub(super) fn fill_rounded_border(
    pixmap: &mut Pixmap,
    border: &RoundedBorderCommand,
    transform: Affine,
) {
    let rect = border.rect;
    if !(rect.width > 0.0 && rect.height > 0.0) {
        return;
    }
    let widths = border
        .widths
        .map(|w| if w.is_finite() { w.max(0.0) } else { 0.0 });
    let [top, right, bottom, left] = widths;
    if widths.iter().all(|w| *w == 0.0) {
        return;
    }
    let outer = capped_radii(rect, border.radii);
    let inner_rect = Rect {
        x: rect.x + left,
        y: rect.y + top,
        width: rect.width - left - right,
        height: rect.height - top - bottom,
    };
    let mut pb = PathBuilder::new();
    push_rounded_rect(&mut pb, rect, outer);
    if inner_rect.width > 0.0 && inner_rect.height > 0.0 {
        let inner = capped_radii(
            inner_rect,
            CornerRadii {
                tl: outer.tl - top.max(left),
                tr: outer.tr - top.max(right),
                br: outer.br - bottom.max(right),
                bl: outer.bl - bottom.max(left),
            },
        );
        push_rounded_rect(&mut pb, inner_rect, inner);
    }
    let Some(ring) = pb.finish() else {
        return;
    };
    let ts = affine_to_ts(transform);
    let colors = border.colors;
    // Sides that show: a zero-width side's colour doesn't matter.
    let shown: Vec<usize> = (0..4).filter(|&i| widths[i] > 0.0).collect();
    if shown.iter().all(|&i| colors[i] == colors[shown[0]]) {
        let color = colors[shown[0]];
        if color.a > 0 {
            pixmap.fill_path(&ring, &solid_paint(color), FillRule::EvenOdd, ts, None);
        }
        return;
    }
    let (l, t) = (rect.x, rect.y);
    let (r, b) = (rect.x + rect.width, rect.y + rect.height);
    let (il, it) = (l + left, t + top);
    let (ir, ib) = (r - right, b - bottom);
    let trapezoids = [
        [(l, t), (r, t), (ir, it), (il, it)],
        [(r, t), (r, b), (ir, ib), (ir, it)],
        [(r, b), (l, b), (il, ib), (ir, ib)],
        [(l, b), (l, t), (il, it), (il, ib)],
    ];
    for &side in &shown {
        let color = colors[side];
        if color.a == 0 {
            continue;
        }
        let mut pb = PathBuilder::new();
        let [first, rest @ ..] = trapezoids[side];
        pb.move_to(first.0, first.1);
        for (x, y) in rest {
            pb.line_to(x, y);
        }
        pb.close();
        let (Some(area), Some(mut mask)) =
            (pb.finish(), Mask::new(pixmap.width(), pixmap.height()))
        else {
            continue;
        };
        mask.fill_path(&area, FillRule::Winding, false, ts);
        pixmap.fill_path(
            &ring,
            &solid_paint(color),
            FillRule::EvenOdd,
            ts,
            Some(&mask),
        );
    }
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
