// Binary entry: parse `argv[1]` (if any) as the initial URL, build the font
// cache, and hand the per-frame closure to `window::run`. All real work lives
// in `mini_browser::state::BrowserState::display_list`.

use mini_browser::{
    render,
    state::{build_font_cache, load_initial_state},
    window,
};

fn main() {
    let mut browser = load_initial_state();
    let mut fonts = build_font_cache(&browser.font_data);
    let mut last_font_count = browser.font_data.len();

    if let Err(error) = window::run("mini-browser", 800, 600, |width, height, input| {
        // Rebuild font cache when navigation loads new fonts. Done before
        // display_list so chrome's caret-width measurement sees the fresh fonts.
        // Glyph cache is keyed by font slot index, so it must be flushed in
        // lockstep — otherwise a slot that now holds a different face would
        // serve bitmaps from the previous one.
        if browser.font_data.len() != last_font_count {
            fonts = build_font_cache(&browser.font_data);
            last_font_count = browser.font_data.len();
            render::invalidate_glyph_cache();
        }

        let commands = browser.display_list(width, height, input, &fonts);

        render::rasterize(&commands, width, height, &fonts)
    }) {
        eprintln!("window error: {error}");
    }
}
