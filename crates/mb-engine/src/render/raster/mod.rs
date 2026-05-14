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
use shapes::{fill_rounded_rect, fill_solid_rect};
use text::draw_text;

pub use text::{measure_text_width, measure_text_wrap, measure_text_wrap_with_family};

use super::{Affine, DisplayCommand};

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
