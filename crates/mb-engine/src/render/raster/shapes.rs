// Rect / rounded-rect rasterisers plus the two tiny tiny-skia conversion
// helpers (`to_ts_rect`, `affine_to_ts`) every other rasteriser in this
// directory uses. Anti-aliasing is forced off on every paint here so the
// pixel-pinned tests at the 4×4 / 5×5 surface scales stay deterministic.

use tiny_skia::{
    FillRule, Mask, Paint, PathBuilder, Pixmap, PixmapPaint, Rect as TsRect,
    Transform as TsTransform,
};

use crate::css::Color;
use crate::layout::Rect;

use super::super::{Affine, CornerRadii, RoundedBorderCommand};
use super::{Scissor, crop, device_rect};

/// A solid rect, limited to `scissor`: axis-aligned rects are intersected
/// with it exactly, rotated ones go through a cropped scratch pixmap.
pub(super) fn fill_solid_rect(
    pixmap: &mut Pixmap,
    color: Color,
    rect: Rect,
    transform: Affine,
    scissor: Scissor,
) {
    if color.a == 0 {
        return;
    }
    if scissor.is_full(pixmap) {
        fill_solid_rect_unclipped(pixmap, color, rect, transform);
    } else if transform.is_axis_aligned() {
        let device = device_rect(rect, transform);
        if let Some(visible) = crate::layout::clip::intersect(device, scissor.rect()) {
            fill_solid_rect_unclipped(pixmap, color, visible, Affine::IDENTITY);
        }
    } else {
        crop(pixmap, rect, transform, scissor, |sub, offset| {
            fill_solid_rect_unclipped(sub, color, rect, offset)
        });
    }
}

fn fill_solid_rect_unclipped(pixmap: &mut Pixmap, color: Color, rect: Rect, transform: Affine) {
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
        fill_solid_rect_unclipped(pixmap, color, rect, transform);
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
    // Per-side colours paint into a scratch layer the size of the box's
    // device bounding box ∩ the pixmap — a tiny-skia mask must match the
    // pixmap it clips, and a full-pixmap mask per side made thousands of
    // small boxes cost boxes × page (F3b). One layer and one mask, reused
    // across sides; the layer is composited once (source-over is
    // associative, so this equals painting the sides directly).
    let Some((ox, oy, w, h)) = device_bounds(rect, transform, pixmap.width(), pixmap.height())
    else {
        return;
    };
    let (Some(mut layer), Some(mut mask)) = (Pixmap::new(w, h), Mask::new(w, h)) else {
        return;
    };
    #[cfg(test)]
    count_scratch_pixels(w, h);
    #[cfg(test)]
    count_scratch_pixels(w, h);
    let local = ts.post_translate(-(ox as f32), -(oy as f32));
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
        let Some(area) = pb.finish() else {
            continue;
        };
        mask.clear();
        mask.fill_path(&area, FillRule::Winding, false, local);
        layer.fill_path(
            &ring,
            &solid_paint(color),
            FillRule::EvenOdd,
            local,
            Some(&mask),
        );
    }
    pixmap.draw_pixmap(
        ox,
        oy,
        layer.as_ref(),
        &PixmapPaint::default(),
        TsTransform::identity(),
        None,
    );
}

/// The integer device-space bounding box of `rect` under `transform`,
/// clipped to a `width` × `height` pixmap: (x, y, w, h), or `None` when
/// empty or not finite.
fn device_bounds(
    rect: Rect,
    transform: Affine,
    width: u32,
    height: u32,
) -> Option<(i32, i32, u32, u32)> {
    let corners = [
        (rect.x, rect.y),
        (rect.x + rect.width, rect.y),
        (rect.x, rect.y + rect.height),
        (rect.x + rect.width, rect.y + rect.height),
    ]
    .map(|(x, y)| transform.apply_point(x, y));
    let min_x = corners
        .iter()
        .map(|c| c.0)
        .fold(f32::INFINITY, f32::min)
        .floor();
    let min_y = corners
        .iter()
        .map(|c| c.1)
        .fold(f32::INFINITY, f32::min)
        .floor();
    let max_x = corners
        .iter()
        .map(|c| c.0)
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil();
    let max_y = corners
        .iter()
        .map(|c| c.1)
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil();
    if !(min_x.is_finite() && min_y.is_finite() && max_x.is_finite() && max_y.is_finite()) {
        return None;
    }
    let x0 = min_x.max(0.0);
    let y0 = min_y.max(0.0);
    let x1 = max_x.min(width as f32);
    let y1 = max_y.min(height as f32);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    // In range by the clamps above (pixmap sides fit u32 / i32).
    Some((x0 as i32, y0 as i32, (x1 - x0) as u32, (y1 - y0) as u32))
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

// Scratch pixels (masks, layers) the rounded-border path allocated on this
// thread — tests bound it by the boxes' area (F3b).
#[cfg(test)]
thread_local! {
    static SCRATCH_PIXELS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn count_scratch_pixels(width: u32, height: u32) {
    SCRATCH_PIXELS.with(|c| c.set(c.get() + u64::from(width) * u64::from(height)));
}

#[cfg(test)]
mod tests {
    use super::super::super::{CornerRadii, DisplayCommand, RoundedBorderCommand};
    use super::SCRATCH_PIXELS;
    use crate::css::Color;
    use crate::layout::Rect;

    /// A page of many small rounded borders with two colours each must cost
    /// work proportional to the boxes, not boxes × page: the per-side
    /// colour split used to allocate a full-pixmap mask per side (F3b — a
    /// tall full-page raster made that a cheap denial of service).
    #[test]
    fn two_colour_rounded_borders_cost_their_own_area() {
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
        let (count, side, page_height) = (2_000usize, 20.0_f32, 40_000usize);
        let commands: Vec<DisplayCommand> = (0..count)
            .map(|i| {
                DisplayCommand::RoundedBorder(RoundedBorderCommand {
                    rect: Rect {
                        x: 10.0,
                        y: i as f32 * side,
                        width: side,
                        height: side,
                    },
                    radii: CornerRadii::uniform(6.0),
                    widths: [3.0; 4],
                    colors: [red, blue, blue, blue],
                })
            })
            .collect();
        SCRATCH_PIXELS.with(|c| c.set(0));
        let started = std::time::Instant::now();
        let pixels = super::super::rasterize(&commands, 400, page_height);
        let elapsed = started.elapsed();
        let scratch = SCRATCH_PIXELS.with(std::cell::Cell::get);
        // A box's device bounding box is at most (side + 2)² pixels (edge
        // rounding); a layer and a mask each.
        let bound = count as u64 * 2 * ((side as u64 + 2) * (side as u64 + 2));
        assert!(
            scratch <= bound,
            "{scratch} scratch pixels for {count} boxes (bound {bound})"
        );
        assert!(elapsed.as_secs() < 20, "took {elapsed:?}");
        // Still painted, both colours: box 1000's top edge red, left blue.
        let at = |x: usize, y: usize| pixels[y * 400 + x] & 0x00FF_FFFF;
        let top = 1_000 * side as usize;
        assert_eq!(at(20, top + 1), 0x00FF_0000);
        assert_eq!(at(11, top + 10), 0x0000_00FF);
    }
}
