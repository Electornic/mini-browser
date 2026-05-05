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

    if let Err(error) = window::run("mini-browser", 800, 600, |width, height, input| {
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

        render::rasterize(&commands, width, height)
    }) {
        eprintln!("window error: {error}");
    }
}
