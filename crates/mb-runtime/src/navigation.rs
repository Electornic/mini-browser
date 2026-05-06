// App-facing document loader and the small set of canned HTML/CSS templates
// the browser falls back to when the network path can't produce a real page
// (load errors, plain-text bodies). Pure functions — no state — so the
// `BrowserState` side stays focused on history/JS lifecycle.

use std::collections::HashMap;

use crate::{html, net, resource};

// Bundle of everything `load_remote_document` produces. The `HashMap<String, String>`
// holds external `<script src>` bodies keyed by the raw `src` attribute string;
// `install_document` looks them up by attribute when walking the DOM, so no extra
// URL resolution is needed at execution time. The trailing `Option<LoadedImage>`
// is the favicon if the page exposed `<link rel="icon">` and the fetch + decode
// succeeded (added in Phase 5.9c).
pub type LoadedDocument = (
    String,
    String,
    HashMap<String, resource::LoadedImage>,
    Vec<Vec<u8>>,
    HashMap<String, String>,
    net::Url,
    Option<resource::LoadedImage>,
);

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
        return Ok((
            document_html,
            stylesheet,
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            final_url,
            None,
        ));
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
    Ok((
        html,
        stylesheets.join("\n"),
        images,
        font_data,
        external_scripts,
        final_url,
        favicon,
    ))
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
