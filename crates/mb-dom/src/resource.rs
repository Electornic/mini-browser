// Pure data + decoder half of the resource layer. The fetching half (which
// reaches into `net::*`) lives in the root crate (will move to `mb-runtime`
// in 4.9d). Engine code only ever reads `LoadedImage`'s pixel buffer, so
// keeping the data type in mb-dom lets the renderer stay below the runtime.

use crate::url::{NetworkError, Url};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceError {
    MissingHref,
    MissingSrc,
    DecodeImage(String),
    Network(NetworkError),
}

impl From<NetworkError> for ResourceError {
    fn from(value: NetworkError) -> Self {
        Self::Network(value)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedImage {
    pub url: Url,
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}

pub fn decode_image(url: Url, bytes: &[u8]) -> Result<LoadedImage, ResourceError> {
    // SVG arrives as a text payload (XML), not an image-crate-compatible
    // raster format, so it has to be sniffed and routed to usvg+resvg
    // before the raster path gets a chance to reject it.
    if looks_like_svg(bytes) {
        return decode_svg(url, bytes);
    }

    // Decode to a simple RGB pixel buffer so rendering does not depend on image crate types.
    let decoded = image::load_from_memory(bytes)
        .map_err(|error| ResourceError::DecodeImage(error.to_string()))?
        .to_rgba8();
    let (width, height) = decoded.dimensions();
    let pixels = decoded
        .pixels()
        .map(|pixel| {
            let [r, g, b, _a] = pixel.0;
            (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
        })
        .collect();

    Ok(LoadedImage {
        url,
        width: width as usize,
        height: height as usize,
        pixels,
    })
}

// SVG sniff: real-world SVG payloads start with either an XML prologue
// (`<?xml ...`), the root tag (`<svg ...`), or a DOCTYPE — possibly behind
// a UTF-8 BOM and leading whitespace. Anything else falls through to the
// raster decoder and surfaces as a normal `DecodeImage` error.
fn looks_like_svg(bytes: &[u8]) -> bool {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let start = bytes
        .iter()
        .position(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        .unwrap_or(bytes.len());
    let trimmed = &bytes[start..];
    trimmed.starts_with(b"<?xml")
        || trimmed.starts_with(b"<svg")
        || trimmed.starts_with(b"<!DOCTYPE svg")
        || trimmed.starts_with(b"<!DOCTYPE SVG")
}

fn decode_svg(url: Url, bytes: &[u8]) -> Result<LoadedImage, ResourceError> {
    use resvg::tiny_skia;
    use resvg::usvg;

    let opts = usvg::Options::default();
    let tree = usvg::Tree::from_data(bytes, &opts)
        .map_err(|error| ResourceError::DecodeImage(error.to_string()))?;
    let size = tree.size().to_int_size();
    let (width, height) = (size.width(), size.height());
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| ResourceError::DecodeImage("svg: zero-sized pixmap".into()))?;
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());

    // tiny-skia hands back premultiplied RGBA. The renderer's draw_image
    // path always paints with alpha=255 (the existing raster format is
    // 0xRRGGBB), so transparent SVG regions are flattened against white
    // here — that matches what a logo on a white page would look like.
    let pixels = pixmap
        .pixels()
        .iter()
        .map(|p| {
            let a = f32::from(p.alpha()) / 255.0;
            let r = f32::from(p.red()) + (1.0 - a) * 255.0;
            let g = f32::from(p.green()) + (1.0 - a) * 255.0;
            let b = f32::from(p.blue()) + (1.0 - a) * 255.0;
            (clamp_u8(r) << 16) | (clamp_u8(g) << 8) | clamp_u8(b)
        })
        .collect();

    Ok(LoadedImage {
        url,
        width: width as usize,
        height: height as usize,
        pixels,
    })
}

fn clamp_u8(value: f32) -> u32 {
    if value <= 0.0 {
        0
    } else if value >= 255.0 {
        255
    } else {
        value.round() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_url() -> Url {
        Url::parse("http://example.com/logo.svg").unwrap()
    }

    #[test]
    fn looks_like_svg_accepts_common_prologues() {
        assert!(looks_like_svg(b"<svg></svg>"));
        assert!(looks_like_svg(b"<?xml version=\"1.0\"?><svg/>"));
        assert!(looks_like_svg(b"\n  <svg/>"));
        assert!(looks_like_svg(b"\xEF\xBB\xBF<svg/>"));
    }

    #[test]
    fn looks_like_svg_rejects_raster() {
        // PNG signature.
        assert!(!looks_like_svg(b"\x89PNG\r\n\x1A\n"));
        // Plain text that is not XML.
        assert!(!looks_like_svg(b"not an image"));
    }

    #[test]
    fn decodes_solid_red_svg() {
        // 2x2 red square — every pixel should land on 0xFF0000 with no
        // alpha bleed once flattened against white.
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><rect width="2" height="2" fill="#ff0000"/></svg>"##;
        let img = decode_image(fake_url(), svg).expect("svg decode");
        assert_eq!(img.width, 2);
        assert_eq!(img.height, 2);
        assert_eq!(img.pixels, vec![0xFF0000; 4]);
    }

    #[test]
    fn flattens_transparent_svg_against_white() {
        // 1x1 fully transparent rect — the fill is meaningless, the alpha
        // is zero, so flattening over white must produce 0xFFFFFF.
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><rect width="1" height="1" fill="#ff0000" fill-opacity="0"/></svg>"##;
        let img = decode_image(fake_url(), svg).expect("svg decode");
        assert_eq!(img.pixels, vec![0xFFFFFF]);
    }

    #[test]
    fn surfaces_invalid_svg_as_decode_error() {
        // Bytes start with `<svg` so they route to the SVG path, but the
        // payload is not parseable XML — must surface as DecodeImage, not
        // panic and not silently fall back to the raster decoder.
        let bogus = b"<svg xmlns=";
        let err = decode_image(fake_url(), bogus).expect_err("must error");
        assert!(matches!(err, ResourceError::DecodeImage(_)));
    }
}
