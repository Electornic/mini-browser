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
//
// This module is the dispatch shell: `rasterize` allocates the pixmap and
// walks the command list; every primitive lives in a sibling file.
//
// `ClipGroup` (compat round 3 step 3) is a scissor rectangle in device
// pixels handed down to every primitive. tiny-skia's `Mask` would only
// reach the tiny-skia paths, and the byte loops above (glyphs, shadows,
// `paint_through`) write `data_mut()` directly — so instead:
//   - byte loops clamp their pixel ranges to the scissor;
//   - a solid rect is intersected with it (exact, the common case);
//   - other tiny-skia shapes that straddle it are drawn into a scratch
//     pixmap the size of (shape ∩ scissor) and copied back — work bounded
//     by the shape's own area, like F3b's layers.
// Fast paths per command: a command whose device bounds sit inside the
// scissor draws unclipped, one entirely outside is skipped.

mod blend;
mod gradient;
mod image;
mod shadow;
mod shapes;
mod text;

use tiny_skia::{Color as TsColor, Pixmap};

use gradient::fill_gradient;
use image::draw_image;
use shadow::fill_box_shadow;
use shapes::{fill_rounded_border, fill_rounded_rect, fill_solid_rect};
use text::draw_text;

pub use text::{
    clear_measure_cache, measure_text_width, measure_text_wrap, measure_text_wrap_with_family,
};

use super::{Affine, DisplayCommand};
use crate::layout::Rect;

/// A device-pixel rectangle the current primitive may write: pixels
/// `x0..x1` × `y0..y1` (half-open), always inside the pixmap. Pixel `i` is
/// inside a clip rect when its centre `i + 0.5` is — so the edges round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Scissor {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl Scissor {
    /// The whole pixmap.
    pub fn full(pixmap: &Pixmap) -> Self {
        Self {
            x0: 0,
            y0: 0,
            x1: i32::try_from(pixmap.width()).unwrap_or(i32::MAX),
            y1: i32::try_from(pixmap.height()).unwrap_or(i32::MAX),
        }
    }

    pub fn is_full(&self, pixmap: &Pixmap) -> bool {
        *self == Self::full(pixmap)
    }

    /// `rect` (device space) ∩ `self`, or `None` when no pixel is left.
    fn narrow(&self, rect: Rect) -> Option<Self> {
        let edge = |v: f32, lo: i32, hi: i32| {
            if v.is_nan() {
                lo
            } else {
                // Clamped to the i32 range of the current scissor first.
                (v.round().clamp(lo as f32, hi as f32)) as i32
            }
        };
        let narrowed = Self {
            x0: edge(rect.x, self.x0, self.x1),
            y0: edge(rect.y, self.y0, self.y1),
            x1: edge(rect.x + rect.width, self.x0, self.x1),
            y1: edge(rect.y + rect.height, self.y0, self.y1),
        };
        (narrowed.x1 > narrowed.x0 && narrowed.y1 > narrowed.y0).then_some(narrowed)
    }

    /// As a float rect, for intersecting with shapes.
    pub fn rect(&self) -> Rect {
        Rect {
            x: self.x0 as f32,
            y: self.y0 as f32,
            width: (self.x1 - self.x0) as f32,
            height: (self.y1 - self.y0) as f32,
        }
    }

    fn contains(&self, bounds: Rect) -> bool {
        bounds.x >= self.x0 as f32
            && bounds.y >= self.y0 as f32
            && bounds.x + bounds.width <= self.x1 as f32
            && bounds.y + bounds.height <= self.y1 as f32
    }

    fn misses(&self, bounds: Rect) -> bool {
        bounds.x + bounds.width <= self.x0 as f32
            || bounds.y + bounds.height <= self.y0 as f32
            || bounds.x >= self.x1 as f32
            || bounds.y >= self.y1 as f32
    }
}

/// Rasterise into a freshly allocated `Vec<u32>`. Convenience for tests
/// and anything that doesn't already own a presentation buffer; production
/// callers should prefer `rasterize_into` so the swizzled output lands
/// directly in the softbuffer surface and the per-frame allocation drops
/// out of the hot path.
pub fn rasterize(commands: &[DisplayCommand], width: usize, height: usize) -> Vec<u32> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let mut buf = vec![0u32; width * height];
    rasterize_into(commands, width, height, &mut buf);
    buf
}

/// Rasterise into a caller-provided `&mut [u32]`. The slice must be at
/// least `width * height` long (any tail beyond that is ignored, any
/// shortfall stops the swizzle early). Each output pixel is packed as
/// `0x00RRGGBB`, matching the legacy `Vec<u32>` contract softbuffer wants.
pub fn rasterize_into(
    commands: &[DisplayCommand],
    width: usize,
    height: usize,
    target: &mut [u32],
) {
    if width == 0 || height == 0 || target.is_empty() {
        return;
    }
    let mut pixmap = Pixmap::new(width as u32, height as u32)
        .expect("non-zero width/height should fit a Pixmap allocation");
    pixmap.fill(TsColor::WHITE);

    let full = Scissor::full(&pixmap);
    for command in commands {
        rasterize_command(&mut pixmap, command, Affine::IDENTITY, full);
    }

    write_pixmap_into(&pixmap, target);
}

// The pixmap's alpha is always 255 (we start with opaque white and every
// blend equation is source-over, which preserves dst.a == 255), so
// premultiplied RGB bytes equal straight RGB bytes — drop the alpha and
// pack as 0x00RRGGBB to match what softbuffer expects. Writing in place
// into the caller's slice skips the intermediate `Vec<u32>` allocation
// that the older `pixmap_to_u32` helper produced every frame.
fn write_pixmap_into(pixmap: &Pixmap, target: &mut [u32]) {
    let data = pixmap.data();
    let pixel_count = (data.len() / 4).min(target.len());
    for (slot, chunk) in target[..pixel_count].iter_mut().zip(data.chunks_exact(4)) {
        *slot = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
    }
}

fn rasterize_command(
    pixmap: &mut Pixmap,
    command: &DisplayCommand,
    transform: Affine,
    scissor: Scissor,
) {
    let clipped = !scissor.is_full(pixmap);
    match command {
        DisplayCommand::SolidRect(color, rect) => {
            fill_solid_rect(pixmap, *color, *rect, transform, scissor)
        }
        DisplayCommand::RoundedRect(color, rect, radii) => {
            if clipped {
                crop(pixmap, *rect, transform, scissor, |sub, offset| {
                    fill_rounded_rect(sub, *color, *rect, *radii, offset)
                });
            } else {
                fill_rounded_rect(pixmap, *color, *rect, *radii, transform)
            }
        }
        DisplayCommand::RoundedBorder(border) => {
            if clipped {
                crop(pixmap, border.rect, transform, scissor, |sub, offset| {
                    fill_rounded_border(sub, border, offset)
                });
            } else {
                fill_rounded_border(pixmap, border, transform)
            }
        }
        DisplayCommand::Text(text) => draw_text(pixmap, text, transform, scissor),
        DisplayCommand::Image(image) => {
            if clipped {
                let rect = Rect {
                    x: image.x,
                    y: image.y,
                    width: image.width,
                    height: image.height,
                };
                crop(pixmap, rect, transform, scissor, |sub, offset| {
                    draw_image(sub, image, offset)
                });
            } else {
                draw_image(pixmap, image, transform)
            }
        }
        DisplayCommand::Gradient(gradient) => {
            if clipped {
                crop(pixmap, gradient.rect, transform, scissor, |sub, offset| {
                    fill_gradient(sub, gradient, offset)
                });
            } else {
                fill_gradient(pixmap, gradient, transform)
            }
        }
        DisplayCommand::BoxShadow(shadow) => fill_box_shadow(pixmap, shadow, transform, scissor),
        DisplayCommand::TransformGroup(group_xform, inner) => {
            // Compose this group's matrix on top of any inherited transform.
            // The display-list builder only emits rotation/shear here (axis-
            // aligned matrices are baked into rect coords up there), so we
            // expect `transform` to be identity in practice — but composing
            // is the right semantics if it ever isn't.
            let composed = transform.compose(*group_xform);
            for cmd in inner {
                rasterize_command(pixmap, cmd, composed, scissor);
            }
        }
        DisplayCommand::ClipGroup(clip, inner) => {
            // The paint walk emits clips in page space, outside any
            // `TransformGroup`; a rotated clip can't be a scissor — draw
            // unclipped rather than wrongly (same rule as the clip table).
            if !transform.is_axis_aligned() {
                for cmd in inner {
                    rasterize_command(pixmap, cmd, transform, scissor);
                }
                return;
            }
            let Some(inner_scissor) = scissor.narrow(device_rect(*clip, transform)) else {
                return;
            };
            let full = Scissor::full(pixmap);
            for cmd in inner {
                match command_bounds(cmd, transform) {
                    Some(bounds) if inner_scissor.contains(bounds) => {
                        rasterize_command(pixmap, cmd, transform, full)
                    }
                    Some(bounds) if inner_scissor.misses(bounds) => {}
                    _ => rasterize_command(pixmap, cmd, transform, inner_scissor),
                }
            }
        }
    }
}

/// `rect` mapped through `transform`, as its axis-aligned bounding box.
fn device_rect(rect: Rect, transform: Affine) -> Rect {
    if transform.is_identity() {
        return rect;
    }
    let corners = [
        (rect.x, rect.y),
        (rect.x + rect.width, rect.y),
        (rect.x, rect.y + rect.height),
        (rect.x + rect.width, rect.y + rect.height),
    ]
    .map(|(x, y)| transform.apply_point(x, y));
    let min_x = corners.iter().map(|c| c.0).fold(f32::INFINITY, f32::min);
    let min_y = corners.iter().map(|c| c.1).fold(f32::INFINITY, f32::min);
    let max_x = corners
        .iter()
        .map(|c| c.0)
        .fold(f32::NEG_INFINITY, f32::max);
    let max_y = corners
        .iter()
        .map(|c| c.1)
        .fold(f32::NEG_INFINITY, f32::max);
    Rect {
        x: min_x,
        y: min_y,
        width: max_x - min_x,
        height: max_y - min_y,
    }
}

/// Where `command` can write, in device space — `None` when not known
/// without shaping (text), which takes the scissored path.
fn command_bounds(command: &DisplayCommand, transform: Affine) -> Option<Rect> {
    let rect = match command {
        DisplayCommand::SolidRect(_, rect) | DisplayCommand::RoundedRect(_, rect, _) => *rect,
        DisplayCommand::RoundedBorder(border) => border.rect,
        DisplayCommand::Gradient(gradient) => gradient.rect,
        DisplayCommand::Image(image) => Rect {
            x: image.x,
            y: image.y,
            width: image.width,
            height: image.height,
        },
        DisplayCommand::BoxShadow(shadow) => {
            let blur = shadow.blur_radius.max(0.0);
            Rect {
                x: shadow.rect.x - blur,
                y: shadow.rect.y - blur,
                width: shadow.rect.width + 2.0 * blur,
                height: shadow.rect.height + 2.0 * blur,
            }
        }
        DisplayCommand::Text(_) => return None,
        DisplayCommand::TransformGroup(matrix, inner) => {
            let composed = transform.compose(*matrix);
            return inner
                .iter()
                .map(|cmd| command_bounds(cmd, composed))
                .try_fold(None::<Rect>, |acc, bounds| {
                    let bounds = bounds?;
                    Some(Some(match acc {
                        None => bounds,
                        Some(acc) => union(acc, bounds),
                    }))
                })
                .flatten();
        }
        DisplayCommand::ClipGroup(clip, _) => *clip,
    };
    let bounds = device_rect(rect, transform);
    (bounds.x.is_finite()
        && bounds.y.is_finite()
        && bounds.width.is_finite()
        && bounds.height.is_finite())
    .then_some(bounds)
}

fn union(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    Rect {
        x,
        y,
        width: (a.x + a.width).max(b.x + b.width) - x,
        height: (a.y + a.height).max(b.y + b.height) - y,
    }
}

/// Draw a tiny-skia shape clipped to `scissor`: into a scratch pixmap the
/// size of (the shape's device bounds ∩ scissor), seeded from and copied
/// back to the same pixels. `draw` gets the scratch pixmap and the matrix to
/// use (`transform` shifted to the scratch origin — an integer translate,
/// so pixel coverage is unchanged).
fn crop(
    pixmap: &mut Pixmap,
    shape: Rect,
    transform: Affine,
    scissor: Scissor,
    draw: impl FnOnce(&mut Pixmap, Affine),
) {
    let Some(region) = scissor.narrow(device_rect(shape, transform)) else {
        return;
    };
    // `narrow` rounds; widen by a pixel each side (still inside the
    // scissor) so anti-aliasing-free edges on half pixels aren't lost.
    let region = Scissor {
        x0: (region.x0 - 1).max(scissor.x0),
        y0: (region.y0 - 1).max(scissor.y0),
        x1: (region.x1 + 1).min(scissor.x1),
        y1: (region.y1 + 1).min(scissor.y1),
    };
    // In range: the region lies inside the pixmap (non-negative, ≤ sides).
    let (x0, y0) = (region.x0 as usize, region.y0 as usize);
    let (w, h) = (
        (region.x1 - region.x0) as usize,
        (region.y1 - region.y0) as usize,
    );
    let Some(mut scratch) = Pixmap::new(w as u32, h as u32) else {
        return;
    };
    let stride = pixmap.width() as usize * 4;
    let row_bytes = w * 4;
    for row in 0..h {
        let src = (y0 + row) * stride + x0 * 4;
        scratch.data_mut()[row * row_bytes..(row + 1) * row_bytes]
            .copy_from_slice(&pixmap.data()[src..src + row_bytes]);
    }
    draw(
        &mut scratch,
        Affine::translate(-(region.x0 as f32), -(region.y0 as f32)).compose(transform),
    );
    for row in 0..h {
        let dst = (y0 + row) * stride + x0 * 4;
        pixmap.data_mut()[dst..dst + row_bytes]
            .copy_from_slice(&scratch.data()[row * row_bytes..(row + 1) * row_bytes]);
    }
}
