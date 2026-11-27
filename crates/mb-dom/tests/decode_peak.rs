//! Peak heap while decoding an image (docs/compat-round3.md Appendix C R4).
//!
//! `decode_image` used to decode into the `image` crate's buffer, copy it
//! to RGBA8 (`to_rgba8`), then copy that into the engine's `Vec<u32>`:
//! two full copies alive at once, ~8 bytes per pixel at the peak for a
//! 4-byte result. A counting global allocator (this test binary only)
//! measures the peak above the starting point.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};

use image::{ImageBuffer, ImageFormat, Luma, Rgb, Rgba};

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards every call to the system allocator unchanged; only
// counts bytes on the side.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let now = CURRENT.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
            PEAK.fetch_max(now, Ordering::SeqCst);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::SeqCst);
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Bytes allocated at the peak of `f`, above what was live before it.
fn peak_during<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let start = CURRENT.load(Ordering::SeqCst);
    PEAK.store(start, Ordering::SeqCst);
    let out = f();
    (out, PEAK.load(Ordering::SeqCst) - start)
}

fn png<P, C>(image: ImageBuffer<P, C>) -> Vec<u8>
where
    P: image::PixelWithColorType,
    [P::Subpixel]: image::EncodableLayout,
    C: std::ops::Deref<Target = [P::Subpixel]>,
{
    let mut out = Cursor::new(Vec::new());
    image
        .write_to(&mut out, ImageFormat::Png)
        .expect("encode png");
    out.into_inner()
}

#[test]
fn decoding_needs_about_one_output_buffer_not_three() {
    const W: u32 = 1200;
    const H: u32 = 900;
    let pixels = (W * H) as f64;
    // Synthetic content only (a gradient with some texture so the PNG isn't
    // trivially tiny); generated, never stored.
    let rgb = png(ImageBuffer::from_fn(W, H, |x, y| {
        Rgb([(x % 251) as u8, (y % 241) as u8, ((x ^ y) % 239) as u8])
    }));
    let rgba = png(ImageBuffer::from_fn(W, H, |x, y| {
        Rgba([
            (x % 251) as u8,
            (y % 241) as u8,
            ((x * y) % 239) as u8,
            (x % 7) as u8,
        ])
    }));
    let grey = png(ImageBuffer::from_fn(W, H, |x, y| {
        Luma([((x + y) % 253) as u8])
    }));
    let url = mb_dom::url::Url::parse("http://synthetic.test/i.png").unwrap();

    let mut report = Vec::new();
    for (name, bytes) in [("rgb8", &rgb), ("rgba8", &rgba), ("l8", &grey)] {
        let (image, peak) =
            peak_during(|| mb_dom::resource::decode_image(url.clone(), bytes).expect("decodes"));
        assert_eq!((image.width, image.height), (W as usize, H as usize));
        let per_pixel = peak as f64 / pixels;
        report.push(format!("{name} {per_pixel:.2} B/px"));
        drop(image);
        // The result itself is 4 B/px; decoder scratch (rows, zlib window)
        // is small. Two full-size copies on top of it was ~8 B/px.
        assert!(
            per_pixel < 5.0,
            "{name}: peak {peak} bytes = {per_pixel:.2} B/px ({report:?})"
        );
    }
    eprintln!("[decode-peak] {}", report.join(", "));
}
