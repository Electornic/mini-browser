use mini_browser::css;

fn main() {
    let path = std::env::args().nth(1).expect("usage: css_diag <path>");
    let source = std::fs::read_to_string(&path).expect("read css");

    let sheet = match css::parse(&source) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("hard parse failure: {e:?}");
            return;
        }
    };

    let nonempty_lines = source
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim().starts_with("/*"))
        .count();

    println!(
        "parsed {} rules from {} non-empty/non-comment source lines",
        sheet.rules.len(),
        nonempty_lines
    );

    for (i, rule) in sheet.rules.iter().enumerate() {
        let sel: Vec<String> = rule.selectors.iter().map(|s| format!("{s:?}")).collect();
        println!("\nrule[{}] selectors: {}", i, sel.join(" || "));
        for d in &rule.declarations {
            println!("  {} = {:?}", d.name, d.value);
        }
    }
}
