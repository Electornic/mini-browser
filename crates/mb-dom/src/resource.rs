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

/// The longest side an SVG is rasterized at — see [`decode_svg`].
pub const MAX_SVG_RASTER_SIDE: usize = 4096;

/// The deepest element nesting an SVG may have (the HTML parser's DOM cap
/// is the same number) — see [`decode_svg`].
const MAX_SVG_ELEMENT_DEPTH: usize = 256;

/// How far (px, after scaling) SVG content may reach before we refuse to
/// render it — see [`decode_svg`].
const MAX_SVG_CANVAS_COORDINATE: f32 = 1e6;

/// The most pixels an SVG's isolated-group layers (filters, opacity,
/// masks) may add up to: two max-size rasters (~130 MB; a blur that size
/// measured ~0.3 s).
const MAX_SVG_LAYER_PIXELS: f64 = 2.0 * (MAX_SVG_RASTER_SIDE * MAX_SVG_RASTER_SIDE) as f64;

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
    // Parse the XML ourselves first (same options as `Tree::from_data`) to
    // bound the nesting: usvg's converter and resvg's renderer recurse per
    // element, and 1 000 nested `<svg>` viewports overflowed the 2 MB stack
    // of the image loader's threads at ~630 levels — an abort, below usvg's
    // own 1 024-level limit (compat round 3 0c). roxmltree itself is
    // iterative.
    let text = std::str::from_utf8(bytes)
        .map_err(|_| ResourceError::DecodeImage("svg: not UTF-8".into()))?;
    let xml = usvg::roxmltree::Document::parse_with_options(
        text,
        usvg::roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        },
    )
    .map_err(|error| {
        ResourceError::DecodeImage(format!("SVG data parsing failed cause {error}"))
    })?;
    if xml_depth(&xml) > MAX_SVG_ELEMENT_DEPTH {
        return Err(ResourceError::DecodeImage(
            "svg: elements nested too deep".into(),
        ));
    }
    let tree = usvg::Tree::from_xmltree(&xml, &opts)
        .map_err(|error| ResourceError::DecodeImage(error.to_string()))?;
    // The document's own width/height is page input: `width="1e9"
    // height="1e9"` is a few bytes asking for a ~4e18-byte pixmap, and an
    // allocation failure aborts (tiny-skia only bounds the row width; the
    // raster decoders have `image`'s 512 MB limit, this path had none).
    // Scale uniformly so the longest side is at most MAX_SVG_RASTER_SIDE —
    // paint scales the image to its box anyway, and layout never reads an
    // image's own size (compat round 3 D2).
    let svg_size = tree.size();
    let longest = svg_size.width().max(svg_size.height());
    let scale = if longest > MAX_SVG_RASTER_SIDE as f32 {
        MAX_SVG_RASTER_SIDE as f32 / longest
    } else {
        1.0
    };
    // `ceil` like usvg's `to_int_size`, so small SVGs keep their old size.
    let raster_side =
        |side: f32| ((side * scale).ceil() as u32).clamp(1, MAX_SVG_RASTER_SIDE as u32);
    let (width, height) = (
        raster_side(svg_size.width()),
        raster_side(svg_size.height()),
    );
    // tiny-skia only rejects paths beyond f32's range; past ~1e9 px its
    // fixed-point scan converter indexes out of bounds, and for some shapes
    // the panic repeats in `SuperBlitter`'s `Drop` while unwinding — a
    // double panic, i.e. an abort no `catch_unwind` stops (release builds;
    // debug hits a `debug_assert` first). Measured: fine at 1e8, broken at
    // 1e9–1e10. Refuse content reaching past MAX_SVG_CANVAS_COORDINATE (100×
    // under that, 244× the raster side) before tiny-skia sees it. The root
    // group's stroke / layer boxes cover every descendant, strokes and
    // filter regions included.
    let root = tree.root();
    let extents = [
        root.abs_stroke_bounding_box(),
        root.abs_layer_bounding_box().to_rect(),
    ];
    let within = |v: f32| v.is_finite() && (v * scale).abs() <= MAX_SVG_CANVAS_COORDINATE;
    if !extents
        .iter()
        .all(|r| within(r.left()) && within(r.top()) && within(r.right()) && within(r.bottom()))
    {
        return Err(ResourceError::DecodeImage(
            "svg: content reaches outside the renderable range".into(),
        ));
    }
    // Every isolating group (filter, opacity, mask, clip) renders into its
    // own layer pixmap, sized from its layer box but clipped only to resvg's
    // `max_filter_bbox` — 5× the canvas per side, 25× its area, ~1.7 GB on a
    // 4096² raster. A blurred 9×9-unit rect on a `viewBox="0 0 1 1"` 4096²
    // image took 3 GB and 13 s (compat round 3 D2 fuzzer). Bound the sum
    // of those layers (an upper bound: nested layers are alive at once).
    let raster_area = f64::from(width) * f64::from(height);
    if isolated_layer_pixels(root, scale, raster_area) > MAX_SVG_LAYER_PIXELS {
        return Err(ResourceError::DecodeImage(
            "svg: content needs larger layers than allowed".into(),
        ));
    }
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| ResourceError::DecodeImage("svg: zero-sized pixmap".into()))?;
    // Second line of defence for the same upstream bugs (compat round 3 D2
    // fuzzer) when the bounds check above doesn't predict them: a panic
    // that *does* unwind makes the SVG an image that failed to decode, like
    // any other bad image, instead of a panic in navigation. Unwind
    // safety: on a panic the half-drawn pixmap is dropped unread.
    let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        resvg::render(
            &tree,
            tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
    }));
    if rendered.is_err() {
        return Err(ResourceError::DecodeImage("svg: renderer panicked".into()));
    }

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

/// The deepest element nesting in `doc`, iteratively.
fn xml_depth(doc: &resvg::usvg::roxmltree::Document) -> usize {
    let mut deepest = 0;
    let mut stack = vec![(doc.root(), 0usize)];
    while let Some((node, depth)) = stack.pop() {
        deepest = deepest.max(depth);
        for child in node.children().filter(|c| c.is_element()) {
            stack.push((child, depth + 1));
        }
    }
    deepest
}

/// Upper bound on the pixels resvg allocates for isolated-group layers:
/// each layer box (scaled to the raster), capped like resvg caps it at 25×
/// the raster area. Iterative — group nesting is page input.
fn isolated_layer_pixels(root: &resvg::usvg::Group, scale: f32, raster_area: f64) -> f64 {
    let per_layer_cap = 25.0 * raster_area;
    let mut total = 0.0;
    let mut stack = vec![root];
    while let Some(group) = stack.pop() {
        if group.should_isolate() {
            let layer = group.abs_layer_bounding_box();
            let area = f64::from(layer.width() * scale) * f64::from(layer.height() * scale);
            total += if area.is_finite() {
                area.min(per_layer_cap)
            } else {
                per_layer_cap
            };
        }
        for node in group.children() {
            if let resvg::usvg::Node::Group(child) = node {
                stack.push(child);
            }
        }
    }
    total
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

    #[test]
    fn huge_svg_is_rasterized_within_the_size_cap() {
        // The pixmap used to be sized straight from the SVG's width/height:
        // a few bytes of `width="1e9" height="1e9"` asked for ~4e18 bytes —
        // an allocation failure is an abort, not a panic. Rasterize at most
        // MAX_SVG_RASTER_SIDE per side, scaled uniformly (paint scales the
        // image to its box anyway).
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="12000" height="6000"><rect width="100%" height="100%" fill="#ff0000"/></svg>"##;
        let img = decode_image(fake_url(), svg).expect("svg decode");
        assert_eq!(
            (img.width, img.height),
            (MAX_SVG_RASTER_SIDE, MAX_SVG_RASTER_SIDE / 2)
        );
        assert_eq!(img.pixels.len(), img.width * img.height);
        assert!(
            img.pixels.iter().all(|p| *p == 0xFF0000),
            "content is scaled, not cropped"
        );

        let bomb = br##"<svg xmlns="http://www.w3.org/2000/svg" width="1e9" height="1e9"/>"##;
        let img = decode_image(fake_url(), bomb).expect("svg decode");
        assert_eq!(
            (img.width, img.height),
            (MAX_SVG_RASTER_SIDE, MAX_SVG_RASTER_SIDE)
        );
        let sliver = br##"<svg xmlns="http://www.w3.org/2000/svg" width="1e9" height="1"/>"##;
        let img = decode_image(fake_url(), sliver).expect("svg decode");
        assert_eq!((img.width, img.height), (MAX_SVG_RASTER_SIDE, 1));
    }

    #[test]
    fn svg_with_absurd_geometry_fails_to_decode_instead_of_panicking() {
        // tiny-skia's path filler indexes out of range / unwraps `None` on
        // coordinates past ~1e9 (release too), and with some shapes the
        // panic repeats in a `Drop` while unwinding — a double panic, i.e.
        // an abort `catch_unwind` can't stop. That's upstream; here such an
        // SVG is just an image that didn't load.
        for svg in [
            // A blur's filter canvas is sized from the filter region, not
            // clipped to the pixmap: 9× overflow on a 4096² raster was a
            // ~37 000² px canvas — 3 GB and 13 s for 250 bytes of SVG.
            &br##"<svg xmlns="http://www.w3.org/2000/svg" width="4096" height="4096" viewBox="0 0 1 1"><filter id="f"><feGaussianBlur stdDeviation="1"/></filter><rect width="9" height="9" filter="url(#f)"/></svg>"##[..],
            &br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="100%" viewBox="0 0 50000 100%"><g transform="scale(1e30) rotate(1e9)"><rect width="1" height="1"/></g></svg>"##[..],
            &br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 1 1"><g transform="scale(1e30) rotate(1e9)"><rect width="1" height="1"/></g></svg>"##[..],
            &br##"<svg xmlns="http://www.w3.org/2000/svg" width="50" height="16" viewBox="0 0 50000 16"><circle r="1e30" stroke-width="1e30" stroke="blue"/></svg>"##[..],
        ] {
            // Rejected before rendering — debug builds hit a `debug_assert`
            // that unwinds cleanly, release builds abort, so "didn't panic
            // here" would prove nothing about release.
            let result = std::panic::catch_unwind(|| decode_image(fake_url(), svg));
            match result {
                Ok(Err(ResourceError::DecodeImage(message))) => {
                    assert!(message.starts_with("svg: content"), "{message}: {}", String::from_utf8_lossy(svg))
                }
                other => panic!("{:?} for {}", other.map(|r| r.map(|i| (i.width, i.height))), String::from_utf8_lossy(svg)),
            }
        }
        // Big but sane content still renders.
        let big = br##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64"><circle r="50000" fill="red"/></svg>"##;
        assert!(decode_image(fake_url(), big).is_ok());
    }

    #[test]
    fn deeply_nested_svg_is_refused_not_a_stack_overflow() {
        // usvg/resvg recurse per element; 1 000 nested `<svg>` viewports
        // overflowed a 2 MB stack (the image loader's scoped threads) at
        // ~630 levels — an abort, below usvg's own 1 024 limit.
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"8\" height=\"8\">{}<rect width=\"9\" height=\"9\"/>{}</svg>",
            "<svg>".repeat(1000),
            "</svg>".repeat(1000)
        );
        let result = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || decode_image(fake_url(), svg.as_bytes()).map(|_| ()))
            .unwrap()
            .join()
            .expect("no panic");
        match result {
            Err(ResourceError::DecodeImage(message)) => {
                assert!(message.contains("deep"), "{message}")
            }
            other => panic!("{other:?}"),
        }
        // Ordinary nesting still decodes.
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"8\" height=\"8\">{}<rect width=\"9\" height=\"9\" fill=\"red\"/>{}</svg>",
            "<g>".repeat(100),
            "</g>".repeat(100)
        );
        let image = decode_image(fake_url(), svg.as_bytes()).expect("100 levels decode");
        assert_eq!(image.pixels[0], 0xFF0000);
    }
}
