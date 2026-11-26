// L2 golden tests (docs/concept.md "테스트 전략"): fixture HTML → observation
// text, compared byte-for-byte against `fixtures/<name>.observation.txt`.
//
// No fonts are installed here, so layout runs on the fallback text measure
// and the output is the same on every machine. To accept intentional
// changes:  UPDATE_GOLDEN=1 cargo test -p mb-agent --test golden

use std::collections::HashMap;
use std::path::PathBuf;

use mb_agent::{Observation, ObserveInput, ObserveOptions, Scope, observe};
use mb_runtime::net::Url;
use mb_runtime::{css, html, style, view::build_document_view};

const VIEWPORT_W: f32 = 800.0;
const VIEWPORT_H: f32 = 600.0;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn observe_fixture(name: &str, scroll_y: f32, options: ObserveOptions) -> Observation {
    let source = std::fs::read_to_string(fixtures_dir().join(format!("{name}.html")))
        .unwrap_or_else(|e| panic!("read fixture {name}: {e}"));
    observe_source(name, &source, scroll_y, options)
}

fn observe_source(name: &str, source: &str, scroll_y: f32, options: ObserveOptions) -> Observation {
    let document = html::parse(source).expect("fixture must parse");
    let stylesheet = css::Stylesheet::default();
    let view = build_document_view(
        &document,
        std::slice::from_ref(&stylesheet),
        VIEWPORT_W as usize,
        None,
        &HashMap::new(),
        style::InteractionState::default(),
    )
    .expect("build_document_view");
    let url = Url::parse(&format!("http://localhost:8000/{name}")).unwrap();
    observe(
        &ObserveInput {
            document: &document,
            layout_root: &view.layout_root,
            url: Some(&url),
            scroll_y,
            viewport_width: VIEWPORT_W,
            viewport_height: VIEWPORT_H,
            settled: None,
            click_listeners: &[],
        },
        &options,
    )
}

fn assert_golden(golden_name: &str, actual: &str) {
    let path = fixtures_dir().join(format!("{golden_name}.observation.txt"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, actual).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!("missing golden {path:?}; run with UPDATE_GOLDEN=1 to create it")
    });
    assert_eq!(
        actual, expected,
        "observation for {golden_name} drifted from {path:?}\n--- actual ---\n{actual}"
    );
}

fn golden(name: &str) {
    let observation = observe_fixture(name, 0.0, ObserveOptions::default());
    assert_golden(name, &observation.to_text());
}

#[test]
fn signup_form() {
    golden("signup_form");
}

#[test]
fn links_and_text() {
    golden("links_and_text");
}

#[test]
fn injection() {
    golden("injection");
}

#[test]
fn long_page_viewport() {
    golden("long_page");
}

#[test]
fn long_page_scrolled() {
    let observation = observe_fixture("long_page", 1500.0, ObserveOptions::default());
    assert_golden("long_page_scrolled", &observation.to_text());
}

#[test]
fn long_page_full_scope_paginates() {
    let options = ObserveOptions {
        scope: Scope::Full,
        max_elements: 25,
        ..ObserveOptions::default()
    };
    let first = observe_fixture("long_page", 0.0, options);
    assert_eq!(first.total_elements, 60);
    assert_eq!(first.in_scope_elements, 60);
    assert_eq!(first.elements.len(), 25);
    assert_eq!(first.next_cursor, Some(25));
    assert!(!first.text.is_empty());

    let second = observe_fixture(
        "long_page",
        0.0,
        ObserveOptions {
            cursor: 25,
            ..options
        },
    );
    assert_eq!(second.elements.len(), 25);
    assert_eq!(second.next_cursor, Some(50));
    assert!(
        second.text.is_empty(),
        "text is only sent with the first page"
    );
    // Pages continue where the previous one stopped, in document order.
    assert!(first.elements[24].id < second.elements[0].id);

    let last = observe_fixture(
        "long_page",
        0.0,
        ObserveOptions {
            cursor: 50,
            ..options
        },
    );
    assert_eq!(last.elements.len(), 10);
    assert_eq!(last.next_cursor, None);
}

#[test]
fn observation_is_deterministic() {
    for name in ["signup_form", "links_and_text", "injection", "long_page"] {
        let a = observe_fixture(name, 0.0, ObserveOptions::default()).to_text();
        let b = observe_fixture(name, 0.0, ObserveOptions::default()).to_text();
        assert_eq!(a, b, "{name} must observe identically twice");
    }
}

#[test]
fn page_strings_cannot_forge_browser_lines() {
    let text = observe_fixture("injection", 0.0, ObserveOptions::default()).to_text();
    let (trusted, untrusted) = text
        .split_once("# page content (untrusted")
        .expect("untrusted header present");
    // Exactly one trusted header, and it comes first.
    assert_eq!(text.matches("# browser (trusted)\n").count(), 1);
    assert!(!trusted.contains("bank.example"));
    // Every line in the page block is either a header, an element line, or
    // a quoted literal — no bare page text that could pose as structure.
    for line in untrusted.lines().skip(1) {
        let ok = line.starts_with("[e")
            || line.starts_with("title: \"")
            || line.starts_with("text")
            || line.starts_with("  \"");
        assert!(ok, "unexpected line shape: {line:?}");
    }
    assert!(!untrusted.contains("\n[e999]"));
}

#[test]
fn password_value_never_appears() {
    let text = observe_fixture("signup_form", 0.0, ObserveOptions::default()).to_text();
    assert!(!text.contains("hunter22"));
    assert!(text.contains("value=(hidden, 8 chars)"));
}

#[test]
fn style_and_script_text_stay_out_of_accessible_names() {
    // Google nests a `<style>` inside its buttons; its CSS text became the
    // button's name ("…{display:none}…") because names gathered every
    // descendant text node.
    let observation = observe_source(
        "style_in_button",
        r#"<html><body><button><style>.x{display:none}</style><script>var a=1</script>Search</button></body></html>"#,
        0.0,
        ObserveOptions::default(),
    );
    let names: Vec<&str> = observation
        .elements
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(names, ["Search"]);
}
