use mini_browser::{css, html, layout, render, style, window};

fn build_sample_display_list(viewport_width: usize) -> Result<Vec<render::DisplayCommand>, String> {
    let sample_html = r#"
        <div id="app" class="page">
            <h1>Mini Browser</h1>
            <p>Hello from the first HTML parser milestone.</p>
        </div>
    "#;
    let sample_css = r#"
        #app {
            width: 320px;
            padding-top: 12px;
            padding-left: 8px;
            background-color: #f0f4f8;
        }
        h1 { font-size: 28px; margin-bottom: 8px; color: #222222; }
        p { color: #0066cc; font-size: 18px; margin-top: 4px; }
    "#;

    let mut nodes = html::parse(sample_html)
        .map_err(|error| format!("html parse error at {}: {}", error.position, error.message))?;
    let stylesheet = css::parse(sample_css)
        .map_err(|error| format!("css parse error at {}: {}", error.position, error.message))?;
    let root = nodes
        .pop()
        .ok_or_else(|| "document did not produce a root node".to_string())?;
    let styled = style::style_tree(&root, &[stylesheet]);
    let layout = layout::layout_tree(&styled, viewport_width as f32);
    Ok(render::build_display_list(&layout))
}

fn main() {
    if let Err(error) = window::run("mini-browser", 800, 600, |width, _height| {
        build_sample_display_list(width).unwrap_or_else(|build_error| {
            eprintln!("{build_error}");
            Vec::new()
        })
    }) {
        eprintln!("window error: {error}");
    }
}
