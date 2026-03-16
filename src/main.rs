use mini_browser::html;

fn main() {
    let sample = r#"
        <div id="app" class="page">
            <h1>Mini Browser</h1>
            <p>Hello from the first HTML parser milestone.</p>
        </div>
    "#;

    match html::parse(sample) {
        Ok(nodes) => println!("{nodes:#?}"),
        Err(error) => eprintln!("parse error at {}: {}", error.position, error.message),
    }
}
