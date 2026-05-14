// Binary entry: parse `argv[1]` (if any) as the initial URL, install the
// shared font system, and hand the per-frame closure to `window::run`. All
// real work lives in `mb_runtime::state::BrowserState::display_list`.

mod window;

use mb_engine::render;
use mb_runtime::state::{install_fonts, load_initial_state};

fn main() {
    let mut browser = load_initial_state();
    install_fonts(&browser.font_data);
    let mut last_font_count = browser.font_data.len();
    // Registering the wake hook on the first frame (not before
    // `window::run`) keeps the EventLoopProxy creation inside the
    // shell — `BrowserState` only sees an `Arc<dyn Fn()>` and stays
    // free of winit dependencies.
    let mut wake_registered = false;

    if let Err(error) = window::run(
        "mini-browser",
         800,
         600,
        |width, height, input, target, wake| {
            if !wake_registered {
                browser.set_navigation_wake(wake.as_arc());
                wake_registered = true;
            }
            // Rebuild font cache when navigation loads new fonts. Done before
            // display_list so chrome's caret-width measurement sees the fresh fonts.
            // `install_fonts` rebuilds the shared swash glyph cache atomically
            // with the FontSystem swap, so callers don't need a separate
            // invalidation step.
            if browser.font_data.len() != last_font_count {
                install_fonts(&browser.font_data);
                last_font_count = browser.font_data.len();
            }

            let commands = browser.display_list(width, height, input);
            // Paint straight into the softbuffer surface to skip the
            // intermediate `Vec<u32>` allocation + memcpy that the
            // previous `rasterize` + caller-copy pattern produced
            // every frame.
            render::rasterize_into(&commands, width, height, target);
            // Caret blink + live JS timers need follow-up frames even
            // with no input. Navigation completion no longer rides here
            // — the worker thread pokes the wake proxy directly when
            // it's done, so the shell sleeps at 0% CPU until then.
            browser.wants_continuous_redraw()
        },
    ) {
        eprintln!("window error: {error}");
    }
}
