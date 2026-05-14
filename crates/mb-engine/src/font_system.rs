// Shaped-text engine + glyph image cache shared across the layout / chrome /
// display-list lanes. The `OnceLock` slots initialise lazily on the first
// `install_fonts` call; later calls swap the inner FontSystem so font
// reloads on navigation propagate. The SwashCache lives as long as the
// FontSystem because its keys reference font ids inside that database.

use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};

use cosmic_text::{FontSystem, SwashCache};

static SHARED_FONT_SYSTEM: OnceLock<Mutex<FontSystem>> = OnceLock::new();
static SHARED_SWASH_CACHE: OnceLock<Mutex<SwashCache>> = OnceLock::new();
// Hash of the most-recently-installed font set (page fonts + macOS
// fallbacks, in order). Lets us skip the FontSystem rebuild when a
// navigation hands us a byte-identical set — keeping the swash glyph
// cache warm across reloads and back/forward moves between same-font
// pages.
static LAST_FONT_HASH: OnceLock<Mutex<Option<u64>>> = OnceLock::new();

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
// Stable content hash over the ordered (page fonts then fallbacks)
// byte slices. `DefaultHasher` is fine here: it's not cryptographic
// but its collision probability is wildly below the practical "two
// font sets that happen to hash the same" threshold, and any rare
// false positive would just mean reusing the swash cache against an
// equally-sized but different font set — bad-looking glyphs, not
// memory safety.
fn compute_font_hash(font_data: &[Vec<u8>], macos_fallbacks: &[Vec<u8>]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // Hash lengths first so a `[a, ab]` set doesn't collide with
    // `[aab, ]` (concatenation ambiguity).
    font_data.len().hash(&mut hasher);
    for data in font_data {
        data.len().hash(&mut hasher);
        data.hash(&mut hasher);
    }
    macos_fallbacks.len().hash(&mut hasher);
    for data in macos_fallbacks {
        data.len().hash(&mut hasher);
        data.hash(&mut hasher);
    }
    hasher.finish()
}

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
    // Hash the page-fonts-plus-fallbacks byte set so a navigation
    // whose fonts are content-identical to the previous install
    // (reload, back/forward to a same-font page, intra-site nav
    // sharing the same `<link rel="stylesheet">` chain) can keep the
    // existing FontSystem + SwashCache. The SwashCache contains
    // already-rasterised glyph images; throwing it away forces every
    // visible glyph through swash again on the first frame after the
    // swap — a multi-hundred-ms first-paint cost on text-heavy pages.
    let new_hash = compute_font_hash(font_data, macos_fallbacks);
    let last_hash_slot =
        LAST_FONT_HASH.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = last_hash_slot.lock()
        && *guard == Some(new_hash)
        && SHARED_FONT_SYSTEM.get().is_some()
    {
        // Identical content + FontSystem already initialised →
        // nothing to do. Skip the FontSystem rebuild and leave
        // the swash glyph cache untouched.
        return;
    }

    let mut fs = FontSystem::new();
    for data in font_data {
        fs.db_mut().load_font_data(data.clone());
    }
    for bytes in macos_fallbacks {
        fs.db_mut().load_font_data(bytes.clone());
    }
    if let Ok(mut guard) = last_hash_slot.lock() {
        *guard = Some(new_hash);
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
