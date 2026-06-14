// Headless profiling harness:
//   1. force MB_PROFILE=1 so the runtime spans hit stderr
//   2. load the given URL through the real `load_remote_document` pipeline
//   3. construct a BrowserState from the loaded doc and drive
//      `display_list` twice — first frame = cold (cache miss), second = warm
//      (cache hit) — so we can see the per-frame cost breakdown
//
// Run with `cargo run --release --example profile_load -- <url>` so the
// numbers reflect what the user actually sees (debug builds dominate on
// glyph + raster work since our own crates stay at opt-level 0).

use std::time::Instant;

use mb_runtime::input::WindowInput;
use mb_runtime::navigation::load_remote_document;
use mb_runtime::state::{BrowserState, install_fonts};

fn main() {
    // SAFETY: single-threaded example; setting an env var before any
    // worker threads exist is sound.
    unsafe {
        std::env::set_var("MB_PROFILE", "1");
    }

    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://news.ycombinator.com/".to_string());

    eprintln!("=== profile_load: {url} ===");

    let load_total = Instant::now();
    let loaded = match load_remote_document(&url) {
        Ok(doc) => doc,
        Err(error) => {
            eprintln!("load failed: {error}");
            std::process::exit(1);
        }
    };
    eprintln!(
        "[mb-profile] navigation::total: {:.2}ms",
        load_total.elapsed().as_secs_f64() * 1000.0
    );

    install_fonts(&loaded.font_data);

    let mut browser = BrowserState::new(
        loaded.final_url.to_string(),
        loaded.document_html,
        loaded.stylesheet,
        loaded.images,
        loaded.font_data,
        loaded.external_scripts,
        Some(loaded.final_url),
        "",
    );

    let input = WindowInput::default();

    eprintln!("=== first frame (cold: style+layout cache miss) ===");
    let _ = browser.display_list(800, 600, &input);

    eprintln!("=== second frame (warm: cache hit expected) ===");
    let _ = browser.display_list(800, 600, &input);
}
