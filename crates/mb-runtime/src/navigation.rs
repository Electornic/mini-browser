// App-facing document loader and the small set of canned HTML/CSS templates
// the browser falls back to when the network path can't produce a real page
// (load errors, plain-text bodies). Pure functions — no state — so the
// `BrowserState` side stays focused on history/JS lifecycle.

use std::collections::HashMap;

use crate::{html, net, resource};

// Bundle of everything `load_remote_document` produces. `external_scripts`
// holds external `<script src>` bodies keyed by the raw `src` attribute
// string; `install_document` looks them up by attribute when walking the
// DOM, so no extra URL resolution is needed at execution time. `favicon`
// is the icon for the tab strip when the page exposed `<link rel="icon">`
// and the fetch + decode succeeded (added in Phase 5.9c).
#[derive(Debug)]
pub struct LoadedDocument {
    pub document_html: String,
    pub stylesheet: String,
    pub images: HashMap<String, resource::LoadedImage>,
    pub font_data: Vec<Vec<u8>>,
    pub external_scripts: HashMap<String, String>,
    pub final_url: net::Url,
    pub favicon: Option<resource::LoadedImage>,
}

pub fn load_remote_document(raw_url: &str) -> Result<LoadedDocument, String> {
    // This function is the app-facing loader. It translates low-level fetch/content-type details
    // into "what document should the browser show?".
    let url = net::Url::parse(raw_url).map_err(|error| format!("url error: {error:?}"))?;
    let fetch_result = net::fetch(&url).map_err(|error| describe_network_error(&error))?;
    let response = fetch_result.response;
    let final_url = fetch_result.final_url;

    if response.status_code != 200 {
        return Err(describe_network_error(&net::NetworkError::HttpStatus(
            response.status_code,
            response.reason_phrase,
        )));
    }

    let content_type = response.header("content-type").unwrap_or("text/html");
    if content_type.starts_with("text/plain") {
        let body = String::from_utf8(response.body)
            .map_err(|_| describe_network_error(&net::NetworkError::InvalidBodyEncoding))?;
        let (document_html, stylesheet) = text_document(&body, &final_url.to_string());
        return Ok(LoadedDocument {
            document_html,
            stylesheet,
            images: HashMap::new(),
            font_data: Vec::new(),
            external_scripts: HashMap::new(),
            final_url,
            favicon: None,
        });
    }

    if !content_type.starts_with("text/html") {
        return Err(format!("unsupported content type {content_type}"));
    }

    let html = String::from_utf8(response.body)
        .map_err(|_| describe_network_error(&net::NetworkError::InvalidBodyEncoding))?;
    let document =
        html::parse(&html).map_err(|error| format!("html parse error {}", error.position))?;
    let stylesheets = resource::load_stylesheets(&document, &final_url)
        .map_err(|error| describe_resource_error(&error))?;
    let font_data = resource::load_fonts(&stylesheets, &final_url);
    let images = resource::load_images(&document, &stylesheets, &final_url)
        .map_err(|error| describe_resource_error(&error))?
        .into_iter()
        .map(|image| (image.url.to_string(), image))
        .collect();
    let external_scripts = resource::load_scripts(&document, &final_url)
        .map_err(|error| describe_resource_error(&error))?;
    // Favicon failures (no `<link rel="icon">`, broken URL, network 404,
    // unsupported codec) silently fall back to "no icon" — the rest of
    // the page is still useful, mirroring how broken stylesheets and
    // missing images degrade.
    let favicon = resource::load_favicon(&document, &final_url);
    Ok(LoadedDocument {
        document_html: html,
        stylesheet: stylesheets.join("\n"),
        images,
        font_data,
        external_scripts,
        final_url,
        favicon,
    })
}

pub fn describe_network_error(error: &net::NetworkError) -> String {
    match error {
        net::NetworkError::UnsupportedScheme(_) => "unsupported scheme".into(),
        net::NetworkError::InvalidUrl(_) => "invalid url".into(),
        net::NetworkError::Io(_) => "network connection failed".into(),
        net::NetworkError::Tls(_) => "tls connection failed".into(),
        net::NetworkError::InvalidResponse(_) => "invalid server response".into(),
        net::NetworkError::MissingLocationHeader => "redirect missing location".into(),
        net::NetworkError::RedirectLimitExceeded => "too many redirects".into(),
        net::NetworkError::HttpStatus(code, _) => format!("http status {code}"),
        net::NetworkError::InvalidBodyEncoding => "invalid response body encoding".into(),
        net::NetworkError::UnexpectedContentType(content_type) => {
            format!("unsupported content type {content_type}")
        }
    }
}

pub fn describe_resource_error(error: &resource::ResourceError) -> String {
    match error {
        resource::ResourceError::MissingHref => "stylesheet missing href".into(),
        resource::ResourceError::MissingSrc => "image missing src".into(),
        resource::ResourceError::DecodeImage(_) => "image decode failed".into(),
        resource::ResourceError::Network(network_error) => describe_network_error(network_error),
    }
}

pub fn error_document(title: &str, message: &str, target: &str) -> (String, String) {
    // Error pages are rendered with the same browser pipeline as normal documents.
    let escaped_title = escape_html(title);
    let escaped_message = escape_html(message);
    let escaped_target = escape_html(target);

    let detail = if escaped_target.is_empty() {
        String::new()
    } else {
        format!("<p>{escaped_target}</p>")
    };

    let html = format!(
        r#"
        <div id="app" class="error">
            <h1>{escaped_title}</h1>
            <p>{escaped_message}</p>
            {detail}
        </div>
    "#
    );

    let css = r#"
        #app {
            width: 520px;
            padding-top: 16px;
            padding-left: 12px;
            background-color: #fff3f0;
        }
        h1 { font-size: 24px; margin-bottom: 8px; color: #8a1c1c; }
        p { font-size: 14px; margin-top: 6px; color: #4a2d2d; }
    "#
    .to_string();

    (html, css)
}

pub fn text_document(body: &str, target: &str) -> (String, String) {
    // `text/plain` is wrapped in a simple HTML shell so the browser can display it without a
    // separate rendering path.
    let escaped_body = escape_html(body);
    let escaped_target = escape_html(target);
    let detail = if escaped_target.is_empty() {
        String::new()
    } else {
        format!("<p>{escaped_target}</p>")
    };

    let html = format!(
        r#"
        <div id="app" class="plain-text">
            <h1>text document</h1>
            {detail}
            <pre>{escaped_body}</pre>
        </div>
    "#
    );

    let css = r#"
        #app {
            width: 680px;
            padding-top: 16px;
            padding-left: 12px;
            background-color: #f7f4ee;
        }
        h1 { font-size: 22px; margin-bottom: 8px; color: #433526; }
        p { font-size: 14px; margin-top: 6px; color: #6b5947; }
        pre {
            margin-top: 10px;
            color: #2f2a24;
        }
    "#
    .to_string();

    (html, css)
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_network_error_maps_each_variant_to_a_stable_phrase() {
        // The status bar paints these strings directly; user-facing
        // text is part of the public contract. Renaming a variant
        // without updating its phrase would silently degrade the
        // message users see on a failed load.
        assert_eq!(
            describe_network_error(&net::NetworkError::UnsupportedScheme("ftp".into())),
            "unsupported scheme"
        );
        assert_eq!(
            describe_network_error(&net::NetworkError::InvalidUrl("::".into())),
            "invalid url"
        );
        assert_eq!(
            describe_network_error(&net::NetworkError::Io("ECONNREFUSED".into())),
            "network connection failed"
        );
        assert_eq!(
            describe_network_error(&net::NetworkError::Tls("handshake".into())),
            "tls connection failed"
        );
        assert_eq!(
            describe_network_error(&net::NetworkError::InvalidResponse("garbage".into())),
            "invalid server response"
        );
        assert_eq!(
            describe_network_error(&net::NetworkError::MissingLocationHeader),
            "redirect missing location"
        );
        assert_eq!(
            describe_network_error(&net::NetworkError::RedirectLimitExceeded),
            "too many redirects"
        );
        assert_eq!(
            describe_network_error(&net::NetworkError::InvalidBodyEncoding),
            "invalid response body encoding"
        );
    }

    #[test]
    fn describe_network_error_embeds_http_status_code_in_message() {
        // HttpStatus is the only variant whose human phrase has to
        // carry numeric context — without the code, "http status" on
        // its own would tell the user nothing diagnostic. The reason
        // phrase is intentionally dropped: most servers send terse or
        // marketing-flavoured reasons that confuse rather than help.
        assert_eq!(
            describe_network_error(&net::NetworkError::HttpStatus(
                404,
                "Not Found".to_string()
            )),
            "http status 404"
        );
        assert_eq!(
            describe_network_error(&net::NetworkError::HttpStatus(500, String::new())),
            "http status 500"
        );
    }

    #[test]
    fn describe_network_error_embeds_unexpected_content_type() {
        // Content-type mismatches happen on every misconfigured server
        // (e.g. `image/jpeg` returned where the link expected HTML);
        // showing the offending value lets the user spot the issue
        // without DevTools.
        assert_eq!(
            describe_network_error(&net::NetworkError::UnexpectedContentType(
                "application/pdf".into()
            )),
            "unsupported content type application/pdf"
        );
    }

    #[test]
    fn describe_resource_error_delegates_network_variant_to_network_describer() {
        // ResourceError wraps NetworkError for fetch failures during
        // sub-resource loads (stylesheets, images, scripts). The
        // resource path therefore inherits the same phrasing as the
        // top-level fetch — anything else would create two divergent
        // glossaries for the same underlying failure.
        let inner = net::NetworkError::HttpStatus(503, "Service Unavailable".into());
        let wrapped = resource::ResourceError::Network(inner);
        assert_eq!(describe_resource_error(&wrapped), "http status 503");
    }

    #[test]
    fn describe_resource_error_maps_local_variants_to_human_strings() {
        assert_eq!(
            describe_resource_error(&resource::ResourceError::MissingHref),
            "stylesheet missing href"
        );
        assert_eq!(
            describe_resource_error(&resource::ResourceError::MissingSrc),
            "image missing src"
        );
        assert_eq!(
            describe_resource_error(&resource::ResourceError::DecodeImage("png".into())),
            "image decode failed"
        );
    }

    #[test]
    fn text_document_escapes_html_special_chars_in_body() {
        // Plain-text bodies routinely contain `<` / `>` / `&` — log
        // files, source code, raw JSON. Without escaping, that text
        // would be parsed as markup and either render as garbage or
        // (worse) execute unintended elements. The wrapper has to
        // neutralise every meta char before injection.
        let (html, _) =
            text_document("<script>alert(1)</script>", "https://example.com/file.txt");
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("&lt;/script&gt;"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn text_document_omits_target_paragraph_when_empty() {
        // An empty target string means the loader couldn't resolve a
        // URL (rare but possible during the bootstrap path). The
        // wrapper must not emit a stray `<p></p>` band — the page
        // would paint an empty highlight strip where the URL ought to be.
        let (html, _) = text_document("hello", "");
        assert!(!html.contains("<p></p>"));
    }

    #[test]
    fn text_document_escapes_target_url_special_chars() {
        // URLs can technically carry `&` (query-string separators), and
        // exotic ones can include `<` / `>` via percent-decoded smuggled
        // values. The display paragraph must escape them so the URL
        // shows as text, not as HTML.
        let (html, _) =
            text_document("body", "https://example.com/?a=1&b=<x>");
        assert!(html.contains("a=1&amp;b=&lt;x&gt;"));
    }

    #[test]
    fn error_document_escapes_title_and_message() {
        // The error path frequently feeds it back text that came from
        // server responses or untrusted input (e.g. a status line). If
        // the wrapper failed to escape, a crafted server message could
        // inject DOM into the user's error page — a low-impact but
        // real XSS-shaped bug.
        let (html, _) = error_document(
            "<bad>",
            "value & \"more\"",
            "javascript:alert('x')",
        );
        assert!(html.contains("&lt;bad&gt;"));
        assert!(html.contains("value &amp; &quot;more&quot;"));
        assert!(html.contains("javascript:alert(&#39;x&#39;)"));
        assert!(!html.contains("<bad>"));
    }

    #[test]
    fn error_document_omits_target_paragraph_when_empty() {
        // Same contract as text_document: no stray `<p></p>` band when
        // there's no target URL to display (the user pressed Enter on
        // an empty address bar).
        let (html, _) = error_document("oops", "details", "");
        assert!(!html.contains("<p></p>"));
    }
}
