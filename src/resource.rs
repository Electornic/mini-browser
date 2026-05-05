use std::collections::HashMap;

use crate::{
    dom::{Document, NodeId, NodeType},
    net::{self, Url},
};

// Resource loading stays separate from HTML parsing so the app can decide when
// to fetch extras. `LoadedImage` + `decode_image` + `ResourceError` now live
// in `mb-dom::resource` so the engine can paint images without depending on
// this fetcher; we re-export here so `crate::resource::LoadedImage` paths
// keep resolving.
pub use mb_dom::resource::{LoadedImage, ResourceError, decode_image};

pub fn load_stylesheets(document: &Document, base_url: &Url) -> Result<Vec<String>, ResourceError> {
    let stylesheet_urls = stylesheet_urls(document, base_url)?;
    // Fetch all stylesheets in parallel; per-resource failures are silently
    // dropped so one broken link does not kill the whole page.
    Ok(parallel_fetch(&stylesheet_urls, |url| net::load_css(url).ok()))
}

// Fetches every `<script src="…">` body referenced by `document` in parallel
// and returns a map from the raw `src` attribute string to the JS body. The
// caller (install_document) walks the DOM in document order and looks up each
// `<script>` element by its `src` attribute, which matches what's stored as
// the key — so no further URL resolution is needed at execution time.
//
// Per-script failures are dropped (the entry is simply absent from the map),
// mirroring how `load_stylesheets` and `load_images` degrade.
pub fn load_scripts(
    document: &Document,
    base_url: &Url,
) -> Result<HashMap<String, String>, ResourceError> {
    let mut pairs = Vec::new();
    for &root in document.roots() {
        collect_script_src_pairs(document, root, base_url, &mut pairs)?;
    }
    if pairs.is_empty() {
        return Ok(HashMap::new());
    }
    // Custom scoped-thread fan-out (rather than reusing `parallel_fetch`)
    // because we need to keep the raw `src` key paired with its fetched body
    // — `parallel_fetch` only carries forward `T` values and would lose that
    // mapping on per-URL failures.
    let fetched: Vec<(String, String)> = std::thread::scope(|scope| {
        let handles: Vec<_> = pairs
            .iter()
            .map(|(raw_src, url)| {
                scope.spawn(move || {
                    net::load_script(url)
                        .ok()
                        .map(|body| (raw_src.clone(), body))
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok().flatten())
            .collect()
    });
    Ok(fetched.into_iter().collect())
}

pub fn load_images(
    document: &Document,
    stylesheets: &[String],
    base_url: &Url,
) -> Result<Vec<LoadedImage>, ResourceError> {
    let mut urls = image_urls(document, base_url)?;
    urls.extend(stylesheet_image_urls(stylesheets, base_url));
    // De-dupe by stringified URL — the same asset is often referenced from
    // both an `<img>` tag and a CSS rule, and fetching twice is a waste.
    let mut seen = std::collections::HashSet::new();
    urls.retain(|url| seen.insert(url.to_string()));
    // Same parallel pattern as stylesheets: failures drop, order is preserved.
    Ok(parallel_fetch(&urls, |url| {
        let bytes = net::load_image(url).ok()?;
        decode_image(url.clone(), &bytes).ok()
    }))
}

// Parses every stylesheet body looking for `background-image: url(...)`
// declarations and resolves each value against `base_url`. Bad URLs and
// unparseable stylesheets are silently skipped; the rest of the page still
// loads, mirroring how broken stylesheet links are tolerated upstream.
fn stylesheet_image_urls(stylesheets: &[String], base_url: &Url) -> Vec<Url> {
    let mut urls = Vec::new();
    for css in stylesheets {
        let Ok(parsed) = crate::css::parse(css) else {
            continue;
        };
        for rule in &parsed.rules {
            for decl in &rule.declarations {
                if decl.name != "background-image" {
                    continue;
                }
                if let crate::css::Value::ImageUrl(raw) = &decl.value
                    && let Ok(resolved) = base_url.resolve(raw)
                {
                    urls.push(resolved);
                }
            }
        }
    }
    urls
}

// Runs `fetch` against every URL on its own scoped thread and collects the
// successful results in original URL order. Failures (None returns / panics
// in worker threads) are filtered out so the rest of the page can still load.
//
// scoped threads keep this safe without `'static` — the fetch closure can
// borrow whatever the caller already had on the stack. There is no thread
// pool: one thread per URL is fine because the bottleneck is network I/O,
// not CPU, and pages rarely reference more than a handful of resources.
fn parallel_fetch<T, F>(urls: &[Url], fetch: F) -> Vec<T>
where
    T: Send,
    F: Fn(&Url) -> Option<T> + Send + Sync,
{
    if urls.is_empty() {
        return Vec::new();
    }
    std::thread::scope(|scope| {
        let fetch = &fetch;
        let handles: Vec<_> = urls
            .iter()
            .map(|url| scope.spawn(move || fetch(url)))
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok().flatten())
            .collect()
    })
}

fn stylesheet_urls(document: &Document, base_url: &Url) -> Result<Vec<Url>, ResourceError> {
    let mut urls = Vec::new();

    for &root in document.roots() {
        collect_stylesheet_urls(document, root, base_url, &mut urls)?;
    }

    Ok(urls)
}

fn image_urls(document: &Document, base_url: &Url) -> Result<Vec<Url>, ResourceError> {
    let mut urls = Vec::new();

    for &root in document.roots() {
        collect_image_urls(document, root, base_url, &mut urls)?;
    }

    Ok(urls)
}

fn collect_stylesheet_urls(
    document: &Document,
    id: NodeId,
    base_url: &Url,
    urls: &mut Vec<Url>,
) -> Result<(), ResourceError> {
    let Some(node) = document.get(id) else {
        return Ok(());
    };
    if let NodeType::Element(element) = &node.node_type {
        // Only explicit stylesheet links are treated as CSS resources.
        if element.tag_name == "link"
            && element
                .attributes
                .get("rel")
                .is_some_and(|value| value.eq_ignore_ascii_case("stylesheet"))
        {
            let href = element
                .attributes
                .get("href")
                .ok_or(ResourceError::MissingHref)?;
            urls.push(base_url.resolve(href)?);
        }
    }

    for &child in &node.children {
        collect_stylesheet_urls(document, child, base_url, urls)?;
    }

    Ok(())
}

fn collect_script_src_pairs(
    document: &Document,
    id: NodeId,
    base_url: &Url,
    out: &mut Vec<(String, Url)>,
) -> Result<(), ResourceError> {
    let Some(node) = document.get(id) else {
        return Ok(());
    };
    if let NodeType::Element(element) = &node.node_type
        && element.tag_name.eq_ignore_ascii_case("script")
    {
        // Inline scripts (no `src`) are handled later by walking the parsed
        // document directly; only external scripts need fetching here.
        if let Some(src) = element.attributes.get("src") {
            out.push((src.clone(), base_url.resolve(src)?));
        }
        // `<script>` content is never nested HTML — don't recurse into it.
        return Ok(());
    }

    for &child in &node.children {
        collect_script_src_pairs(document, child, base_url, out)?;
    }

    Ok(())
}

fn collect_image_urls(
    document: &Document,
    id: NodeId,
    base_url: &Url,
    urls: &mut Vec<Url>,
) -> Result<(), ResourceError> {
    let Some(node) = document.get(id) else {
        return Ok(());
    };
    if let NodeType::Element(element) = &node.node_type {
        // Images are fetched lazily from DOM attributes after the main document is parsed.
        if element.tag_name == "img" {
            let src = element
                .attributes
                .get("src")
                .ok_or(ResourceError::MissingSrc)?;
            urls.push(base_url.resolve(src)?);
        }
    }

    for &child in &node.children {
        collect_image_urls(document, child, base_url, urls)?;
    }

    Ok(())
}

pub fn load_fonts(css_sources: &[String], base_url: &Url) -> Vec<Vec<u8>> {
    // Resolve every @font-face URL up front, then fetch them all in parallel
    // through the shared connection pool — same-origin font files reuse the
    // already-warmed TLS session that the stylesheet fetch primed.
    let urls: Vec<Url> = css_sources
        .iter()
        .flat_map(|css| extract_font_urls(css))
        .filter_map(|url_str| {
            if url_str.contains("://") {
                Url::parse(&url_str).ok()
            } else {
                base_url.resolve(&url_str).ok()
            }
        })
        .collect();
    parallel_fetch(&urls, |url| match net::fetch(url) {
        Ok(result) if result.response.status_code == 200 => Some(result.response.body),
        _ => None,
    })
}

fn extract_font_urls(css: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let lower = css.to_ascii_lowercase();
    let mut search_pos = 0;

    while let Some(offset) = lower[search_pos..].find("@font-face") {
        let abs_pos = search_pos + offset;
        let after_keyword = abs_pos + "@font-face".len();

        // Find the opening '{' of the @font-face block.
        let brace_start = match css[after_keyword..].find('{') {
            Some(pos) => after_keyword + pos + 1,
            None => break,
        };

        // Find the matching '}'.
        let mut depth = 1;
        let mut block_end = brace_start;
        for (i, ch) in css[brace_start..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        block_end = brace_start + i;
                        break;
                    }
                }
                _ => {}
            }
        }

        let block = &css[brace_start..block_end];
        if let Some(url) = extract_url_value(block) {
            urls.push(url);
        }

        search_pos = block_end + 1;
    }

    urls
}

fn extract_url_value(block: &str) -> Option<String> {
    // Collect all url() values from the block, then pick the best format the font system loads.
    let mut urls = Vec::new();
    let lower = block.to_ascii_lowercase();
    let mut search_pos = 0;

    while let Some(url_offset) = lower[search_pos..].find("url(") {
        let start = search_pos + url_offset + 4;
        let rest = &block[start..];

        let (url, skip) = if rest.starts_with('"') || rest.starts_with('\'') {
            let quote = rest.as_bytes()[0] as char;
            let inner = &rest[1..];
            match inner.find(quote) {
                Some(end) => (&inner[..end], start + end + 2),
                None => break,
            }
        } else {
            match rest.find(')') {
                Some(end) => (rest[..end].trim(), start + end),
                None => break,
            }
        };

        if !url.is_empty() {
            urls.push(url.to_string());
        }
        search_pos = skip + 1;
    }

    // Only pick TTF/OTF — what cosmic-text's fontdb backend can ingest directly.
    urls.into_iter().find(|u| {
        let l = u.to_ascii_lowercase();
        l.ends_with(".ttf") || l.ends_with(".otf")
    })
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use crate::{html, net::Url};

    use super::{load_images, load_scripts, load_stylesheets};

    #[test]
    fn resolves_stylesheet_links_from_document() {
        let document = html::parse(
            r#"
                <html>
                    <head>
                        <link rel="stylesheet" href="/base.css" />
                        <link rel="stylesheet" href="nested/site.css" />
                    </head>
                </html>
            "#,
        )
        .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 1024];
                let bytes_read = stream.read(&mut request).unwrap();
                let request_text = String::from_utf8_lossy(&request[..bytes_read]);
                let body = if request_text.starts_with("GET /base.css HTTP/1.1") {
                    "body { color: #111111; }"
                } else if request_text.starts_with("GET /articles/nested/site.css HTTP/1.1") {
                    "p { color: #222222; }"
                } else {
                    panic!("unexpected request: {request_text}");
                };

                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nConnection: close\r\n\r\n{body}"
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });

        let base_url = Url::parse(&format!("http://127.0.0.1:{port}/articles/index.html")).unwrap();
        let stylesheets = load_stylesheets(&document, &base_url).unwrap();

        server.join().unwrap();
        assert_eq!(stylesheets.len(), 2);
        assert_eq!(stylesheets[0], "body { color: #111111; }");
        assert_eq!(stylesheets[1], "p { color: #222222; }");
    }

    #[test]
    fn fetches_external_scripts_keyed_by_raw_src() {
        // Two `<script src>` references — one absolute path, one relative —
        // both resolve through the document's base URL and the returned map
        // keys back the original raw `src` attributes (so the install_document
        // walker can look them up directly while traversing the DOM).
        let document = html::parse(
            r#"
                <html>
                    <head>
                        <script src="/lib.js"></script>
                        <script src="nested/page.js"></script>
                    </head>
                </html>
            "#,
        )
        .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 1024];
                let bytes_read = stream.read(&mut request).unwrap();
                let request_text = String::from_utf8_lossy(&request[..bytes_read]);
                let body = if request_text.starts_with("GET /lib.js HTTP/1.1") {
                    "var lib = 1;"
                } else if request_text.starts_with("GET /pages/nested/page.js HTTP/1.1") {
                    "var page = 2;"
                } else {
                    panic!("unexpected request: {request_text}");
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nConnection: close\r\n\r\n{body}"
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });

        let base_url =
            Url::parse(&format!("http://127.0.0.1:{port}/pages/index.html")).unwrap();
        let scripts = load_scripts(&document, &base_url).unwrap();

        server.join().unwrap();
        assert_eq!(scripts.len(), 2);
        assert_eq!(scripts.get("/lib.js").map(String::as_str), Some("var lib = 1;"));
        assert_eq!(
            scripts.get("nested/page.js").map(String::as_str),
            Some("var page = 2;"),
        );
    }

    #[test]
    fn load_scripts_returns_empty_map_when_document_has_no_external_scripts() {
        // Inline `<script>` (no `src`) must not appear in the externals map —
        // those are handled by the DOM walker reading text-child contents.
        let document = html::parse("<html><body><script>var x = 1;</script></body></html>").unwrap();
        let base = Url::parse("http://example.com/").unwrap();
        let scripts = load_scripts(&document, &base).unwrap();
        assert!(scripts.is_empty());
    }

    #[test]
    fn loads_image_resources_from_document() {
        let document = html::parse(
            r#"
                <html>
                    <body>
                        <img src="/pixel.png" />
                    </body>
                </html>
            "#,
        )
        .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let bytes_read = stream.read(&mut request).unwrap();
            let request_text = String::from_utf8_lossy(&request[..bytes_read]);
            assert!(request_text.starts_with("GET /pixel.png HTTP/1.1"));

            let png: &[u8] = &[
                0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
                0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
                0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
                0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0xF0, 0x1F, 0x00, 0x05, 0x00, 0x01, 0xFF, 0x89, 0x99,
                0x3D, 0x1D, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
            ];
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                png.len()
            );
            stream.write_all(header.as_bytes()).unwrap();
            stream.write_all(png).unwrap();
        });

        let base_url = Url::parse(&format!("http://127.0.0.1:{port}/index.html")).unwrap();
        let images = load_images(&document, &[], &base_url).unwrap();

        server.join().unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].width, 1);
        assert_eq!(images[0].height, 1);
        assert_eq!(images[0].pixels, vec![0xFF0000]);
    }
}
