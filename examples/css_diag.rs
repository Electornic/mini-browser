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
        // The selector AST is now opaque (owned by the `selectors` crate),
        // so the diagnostic just prints the parsed list count + max
        // specificity instead of the per-component dump it used to emit.
        println!(
            "\nrule[{}] {} selector branch(es), specificity={}",
            i,
            rule.selectors.list().len(),
            rule.selectors.specificity()
        );
        for d in &rule.declarations {
            println!("  {} = {:?}", d.name, d.value);
        }
    }
}
