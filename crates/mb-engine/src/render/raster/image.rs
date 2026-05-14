// Image rasteriser. Both `<img>` and `background-image: url(...)` route
// through here. The source pixels are staged into a tiny-skia tile
// pixmap once, then a `Pattern` shader handles the scaling / rotation /
// sprite slicing natively. Two shapes are emitted depending on the
// `ImageCommand` payload:
//   - stretch: fill the box from the full source, scaled to the dest
//   - sprite: render at native size with the source origin shifted by
//     (source_x, source_y) so the box reveals only that slice

use tiny_skia::{
    FilterQuality, Paint, Pattern, Pixmap, Rect as TsRect, SpreadMode, Transform as TsTransform,
};

use super::super::{Affine, ImageCommand};
use super::shapes::affine_to_ts;

pub(super) fn draw_image(pixmap: &mut Pixmap, image: &ImageCommand, transform: Affine) {
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
