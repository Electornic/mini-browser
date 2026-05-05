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

// Loads `font_data` (web fonts the page declared) plus a macOS system font
// fallback into the shared cosmic-text `FontSystem` and rebuilds its swash
// glyph cache. `main` calls this at startup and again whenever navigation
// brings in new fonts; downstream measure / paint paths read the shared
// slots directly so no font handle has to be threaded through.
pub fn install_fonts(font_data: &[Vec<u8>]) {
    let macos_fallback = std::fs::read("/System/Library/Fonts/AppleSDGothicNeo.ttc").ok();
    install_shared_font_system(font_data, macos_fallback.as_deref());
}

fn install_shared_font_system(font_data: &[Vec<u8>], macos_fallback: Option<&[u8]>) {
    let mut fs = FontSystem::new();
    for data in font_data {
        fs.db_mut().load_font_data(data.clone());
    }
    if let Some(bytes) = macos_fallback {
        fs.db_mut().load_font_data(bytes.to_vec());
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
