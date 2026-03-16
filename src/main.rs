use mini_browser::{css, html, layout, style};

fn main() {
    let sample_html = r#"
        <div id="app" class="page">
            <h1>Mini Browser</h1>
            <p>Hello from the first HTML parser milestone.</p>
        </div>
    "#;
    let sample_css = r#"
        #app { width: 320px; padding-top: 12px; }
        h1 { font-size: 28px; margin-bottom: 8px; }
        p { color: #0066cc; font-size: 18px; margin-top: 4px; }
    "#;

    match (html::parse(sample_html), css::parse(sample_css)) {
        (Ok(mut nodes), Ok(stylesheet)) => {
            if let Some(root) = nodes.pop() {
                let styled = style::style_tree(&root, &[stylesheet]);
                let layout = layout::layout_tree(&styled, 800.0);
                println!("{layout:#?}");
            }
        }
        (Err(error), _) => eprintln!("html parse error at {}: {}", error.position, error.message),
        (_, Err(error)) => eprintln!("css parse error at {}: {}", error.position, error.message),
    }
}
