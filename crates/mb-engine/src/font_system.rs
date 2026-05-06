// Shaped-text engine + glyph image cache shared across the layout / chrome /
// display-list lanes. The `OnceLock` slots initialise lazily on the first
// `install_fonts` call; later calls swap the inner FontSystem so font
// reloads on navigation propagate. The SwashCache lives as long as the
// FontSystem because its keys reference font ids inside that database.

use std::sync::{Mutex, OnceLock};

use cosmic_text::{FontSystem, SwashCache};

static SHARED_FONT_SYSTEM: OnceLock<Mutex<FontSystem>> = OnceLock::new();
static SHARED_SWASH_CACHE: OnceLock<Mutex<SwashCache>> = OnceLock::new();

pub fn shared_font_system() -> Option<&'static Mutex<FontSystem>> {
    SHARED_FONT_SYSTEM.get()
}

pub fn shared_swash_cache() -> Option<&'static Mutex<SwashCache>> {
    SHARED_SWASH_CACHE.get()
}

// Loads `font_data` (web fonts the page declared) plus the macOS system
// fallbacks (one proportional, one monospace) into the shared cosmic-text
// `FontSystem` and rebuilds its swash glyph cache. `main` calls this at
// startup and again whenever navigation brings in new fonts; downstream
// measure / paint paths read the shared slots directly so no font handle
// has to be threaded through.
//
// Two fallbacks are installed because cosmic-text's `Family::Monospace`
// query only finds something useful if the FontSystem's database actually
// contains a font with the monospace OS/2 bit. AppleSDGothicNeo covers
// the proportional sans-serif chain; Menlo is the historically stable
// macOS monospace face (kept around long after SF Mono shipped) and
// satisfies the monospace query that `<pre>` / `<code>` runs go through.
// Missing files are silently skipped — useful for non-macOS hosts and CI
// (the toy fallback path inside `measure_text_wrap` still produces a
// reasonable estimate).
pub fn install_fonts(font_data: &[Vec<u8>]) {
    let macos_fallbacks: Vec<Vec<u8>> = [
        "/System/Library/Fonts/AppleSDGothicNeo.ttc",
        "/System/Library/Fonts/Menlo.ttc",
    ]
    .iter()
    .filter_map(|path| std::fs::read(path).ok())
    .collect();
    install_shared_font_system(font_data, &macos_fallbacks);
}

fn install_shared_font_system(font_data: &[Vec<u8>], macos_fallbacks: &[Vec<u8>]) {
    let mut fs = FontSystem::new();
    for data in font_data {
        fs.db_mut().load_font_data(data.clone());
    }
    for bytes in macos_fallbacks {
        fs.db_mut().load_font_data(bytes.clone());
    }
    match SHARED_FONT_SYSTEM.get() {
        Some(slot) => {
            if let Ok(mut guard) = slot.lock() {
                *guard = fs;
            }
        }
        None => {
            let _ = SHARED_FONT_SYSTEM.set(Mutex::new(fs));
        }
    }
    // Glyph image cache keys reference font ids inside the new FontSystem, so
    // any reload that swaps the FontSystem must drop the cached images that
    // pointed at the previous one. Re-create the cache rather than mutating in
    // place — SwashCache exposes no `clear` API.
    match SHARED_SWASH_CACHE.get() {
        Some(slot) => {
            if let Ok(mut guard) = slot.lock() {
                *guard = SwashCache::new();
            }
        }
        None => {
            let _ = SHARED_SWASH_CACHE.set(Mutex::new(SwashCache::new()));
        }
    }
}
