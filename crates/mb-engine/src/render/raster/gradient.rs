// `Gradient` rasteriser. Both `linear-gradient` and `radial-gradient`
// route through tiny-skia's shader API: linear pins start/end to the
// box's main axis; radial uses a unit-circle gradient with
// `gradient_transform = scale(rx, ry).post_translate(cx, cy)` so an
// elliptical falloff matches the box aspect.

use tiny_skia::{
    Color as TsColor, GradientStop, LinearGradient, Paint, Pixmap, Point as TsPoint,
    RadialGradient, Rect as TsRect, SpreadMode, Transform as TsTransform,
};

use crate::css::{GradientDirection, GradientKind};

use super::super::{Affine, GradientCommand};
use super::shapes::affine_to_ts;

pub(super) fn fill_gradient(pixmap: &mut Pixmap, gradient: &GradientCommand, transform: Affine) {
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
