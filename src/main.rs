use std::env;

use mini_browser::{css, html, layout, net, render, resource, style, window};

fn build_display_list(
    document_html: &str,
    stylesheet_source: &str,
    viewport_width: usize,
) -> Result<Vec<render::DisplayCommand>, String> {
    let mut nodes = html::parse(document_html)
        .map_err(|error| format!("html parse error at {}: {}", error.position, error.message))?;
    let stylesheet = css::parse(stylesheet_source)
        .map_err(|error| format!("css parse error at {}: {}", error.position, error.message))?;
    let root = nodes
        .pop()
        .ok_or_else(|| "document did not produce a root node".to_string())?;
    let styled = style::style_tree(&root, &[stylesheet]);
    let layout = layout::layout_tree(&styled, viewport_width as f32);
    Ok(render::build_display_list(&layout))
}

fn sample_html() -> &'static str {
    r#"
        <div id="app" class="page">
            <h1>Mini Browser</h1>
            <p>Hello from the first HTML parser milestone.</p>
        </div>
    "#
}

fn sample_css() -> &'static str {
    r#"
        #app {
            width: 320px;
            padding-top: 12px;
            padding-left: 8px;
            background-color: #f0f4f8;
        }
        h1 { font-size: 28px; margin-bottom: 8px; color: #222222; }
        p { color: #0066cc; font-size: 18px; margin-top: 4px; }
    "#
}

fn load_document_from_args() -> Result<(String, String), String> {
    match env::args().nth(1) {
        Some(raw_url) => {
            let url = net::Url::parse(&raw_url).map_err(|error| format!("url error: {error:?}"))?;
            let html = net::load_html(&url).map_err(|error| format!("network error: {error:?}"))?;
            let nodes = html::parse(&html).map_err(|error| {
                format!("html parse error at {}: {}", error.position, error.message)
            })?;
            let stylesheets = resource::load_stylesheets(&nodes, &url)
                .map_err(|error| format!("resource error: {error:?}"))?;
            Ok((html, stylesheets.join("\n")))
        }
        None => Ok((sample_html().to_string(), sample_css().to_string())),
    }
}

fn main() {
    let (document_html, stylesheet) = load_document_from_args().unwrap_or_else(|error| {
        eprintln!("{error}");
        (sample_html().to_string(), sample_css().to_string())
    });

    if let Err(error) = window::run("mini-browser", 800, 600, |width, _height| {
        build_display_list(&document_html, &stylesheet, width).unwrap_or_else(|build_error| {
            eprintln!("{build_error}");
            Vec::new()
        })
    }) {
        eprintln!("window error: {error}");
    }
}
