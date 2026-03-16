use mini_browser::{css, html, style};

fn main() {
    let sample_html = r#"
        <div id="app" class="page">
            <h1>Mini Browser</h1>
            <p>Hello from the first HTML parser milestone.</p>
        </div>
    "#;
    let sample_css = r#"
        #app { color: #222222; font-size: 18px; }
        p { color: #0066cc; }
    "#;

    match (html::parse(sample_html), css::parse(sample_css)) {
        (Ok(mut nodes), Ok(stylesheet)) => {
            if let Some(root) = nodes.pop() {
                let styled = style::style_tree(&root, &[stylesheet]);
                println!("{styled:#?}");
            }
        }
        (Err(error), _) => eprintln!("html parse error at {}: {}", error.position, error.message),
        (_, Err(error)) => eprintln!("css parse error at {}: {}", error.position, error.message),
    }
}
