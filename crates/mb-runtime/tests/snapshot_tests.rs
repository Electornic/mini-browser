// Pixel-level snapshot regression tests for the rendering pipeline.
//
// Each test loads an HTML fixture, builds a DocumentView (bypassing the
// browser chrome to avoid the system-font dependency), rasterises it,
// and compares the resulting pixel buffer byte-for-byte against a
// committed PNG baseline. The first run for a missing baseline writes
// it and reports "baseline created"; later runs fail on any pixel
// mismatch. Refresh with `UPDATE_SNAPSHOTS=1 cargo test snapshot_`.
//
// Fixtures must be text-free for now — cosmic-text reaches for system
// fonts which vary across machines, so any text-rendering fixture
// would flake on a different dev box. When Phase 7 needs text
// fixtures, bundle a deterministic font and install it before
// building the document view.

use std::collections::HashMap;
use std::path::PathBuf;

use mb_runtime::{css, display_list::build_document_view, html, render, style};

const VIEWPORT_W: usize = 320;
const VIEWPORT_H: usize = 240;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/fixtures")
}

fn baselines_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/baselines")
}

// Drive the page-area pipeline (parse → style → layout → paint)
// without the BrowserState chrome wrapper, so fonts only matter when
// the fixture itself paints text. Returns one 0x00RRGGBB pixel per
// VIEWPORT_W * VIEWPORT_H slot — the same buffer shape softbuffer
// would receive from main.rs.
fn render_fixture(fixture_name: &str) -> Vec<u32> {
    let html_path = fixtures_dir().join(format!("{fixture_name}.html"));
    let html_source = std::fs::read_to_string(&html_path)
        .unwrap_or_else(|e| panic!("read fixture {html_path:?}: {e}"));

    let document = html::parse(&html_source).expect("fixture html must parse");
    // The document carries its own <style> block; the cascade reads
    // both inline `style=` attributes and any author stylesheet we
    // pass in. We pass an empty list and let the document-internal
    // stylesheet drive everything, which matches how `BrowserState`
    // hands its stylesheet to the engine.
    let stylesheet = css::Stylesheet::default();
    let images: HashMap<String, mb_runtime::resource::LoadedImage> = HashMap::new();
    let interaction = style::InteractionState::default();

    let view = build_document_view(
        &document,
        &stylesheet,
        VIEWPORT_W,
        None,
        &images,
        interaction,
    )
    .expect("build_document_view");

    render::rasterize(&view.commands, VIEWPORT_W, VIEWPORT_H)
}

// softbuffer packs each pixel as 0x00RRGGBB inside a u32. Convert to
// RGBA8 byte order so the `image` crate can read/write standard PNGs.
fn pixels_to_rgba_bytes(pixels: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixels.len() * 4);
    for &p in pixels {
        let r = ((p >> 16) & 0xFF) as u8;
        let g = ((p >> 8) & 0xFF) as u8;
        let b = (p & 0xFF) as u8;
        out.extend_from_slice(&[r, g, b, 0xFF]);
    }
    out
}

fn assert_snapshot(name: &str, pixels: &[u32]) {
    let baseline_path = baselines_dir().join(format!("{name}.png"));
    std::fs::create_dir_all(baselines_dir()).expect("create baselines dir");

    let actual_bytes = pixels_to_rgba_bytes(pixels);
    let actual = image::RgbaImage::from_raw(VIEWPORT_W as u32, VIEWPORT_H as u32, actual_bytes)
        .expect("pixel buffer matches viewport dimensions");

    let update = std::env::var("UPDATE_SNAPSHOTS").is_ok();
    let missing = !baseline_path.exists();

    if missing || update {
        actual
            .save(&baseline_path)
            .unwrap_or_else(|e| panic!("save baseline {baseline_path:?}: {e}"));
        if missing && !update {
            // First-time test runs auto-create their baseline so the
            // initial commit doesn't need a separate "generate
            // baselines" step. The test still passes — the caller
            // sees the new file in `git status` and reviews it.
            eprintln!("[snapshot] baseline created: {baseline_path:?}");
            return;
        }
    }

    let expected = image::open(&baseline_path)
        .unwrap_or_else(|e| panic!("open baseline {baseline_path:?}: {e}"))
        .to_rgba8();
    assert_eq!(
        actual.dimensions(),
        expected.dimensions(),
        "snapshot dim mismatch for {name}"
    );

    let mut first_diff: Option<(u32, u32, [u8; 4], [u8; 4])> = None;
    let mut diff_count: usize = 0;
    for (i, (a, e)) in actual.pixels().zip(expected.pixels()).enumerate() {
        if a.0 != e.0 {
            diff_count += 1;
            if first_diff.is_none() {
                let x = (i % VIEWPORT_W) as u32;
                let y = (i / VIEWPORT_W) as u32;
                first_diff = Some((x, y, a.0, e.0));
            }
        }
    }
    if diff_count > 0 {
        let (x, y, a, e) = first_diff.unwrap();
        panic!(
            "snapshot mismatch for {name}: {diff_count} pixel(s) differ; first at ({x},{y}) actual={a:?} expected={e:?}. \
             Inspect {baseline_path:?} and run with UPDATE_SNAPSHOTS=1 to refresh if intended.",
        );
    }
}

#[test]
fn snapshot_solid_color_box() {
    let pixels = render_fixture("solid_color_box");
    assert_snapshot("solid_color_box", &pixels);
}

#[test]
fn snapshot_flex_layout() {
    let pixels = render_fixture("flex_layout");
    assert_snapshot("flex_layout", &pixels);
}

#[test]
fn snapshot_border_radius() {
    let pixels = render_fixture("border_radius");
    assert_snapshot("border_radius", &pixels);
}
