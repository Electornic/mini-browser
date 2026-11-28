// compat round 4 step 0 P4: the script-error rules on hand-written console
// lines (the shapes `console.txt` holds), against the frozen list file.

#[allow(dead_code)]
#[path = "../examples/compat_support/classify.rs"]
mod classify;

use classify::{ApiList, ErrorKind, classify_lines, kind_of, message_of};

fn list() -> ApiList {
    ApiList::parse(include_str!("../../../scripts/compat-js-apis.tsv"))
}

#[test]
fn list_file_parses_with_apis_and_cascades() {
    let list = list();
    assert!(list.len() > 50, "{}", list.len());
    assert_eq!(
        kind_of("jQuery is not defined", &list, true),
        ErrorKind::Cascade("jQuery".into())
    );
}

#[test]
fn missing_api_from_not_a_function_and_not_defined() {
    let list = list();
    let lines = [
        "Uncaught in script: not a function (near \"d.getElementsByTagName\") at https://cdn.test/jquery-1.9.1.min.js:3:10388",
        "Uncaught in script: ReferenceError: customElements is not defined at https://a.test/#inline-2:1:5",
        "Uncaught in timer callback: TypeError: not a function (near \"window.matchMedia\") at https://a.test/app.js:9:1",
    ];
    let verdict = classify_lines(&lines, &list);
    assert_eq!(
        verdict.first_missing_api.as_deref(),
        Some("getElementsByTagName")
    );
    assert_eq!(
        verdict.first_error.as_deref(),
        Some("not a function (near \"d.getElementsByTagName\")")
    );
    assert_eq!(
        verdict.any_missing_api,
        ["getElementsByTagName", "customElements", "matchMedia"]
    );
    assert_eq!(verdict.kinds(), "missing-api:3,page-bug:0,cascade:0");
}

#[test]
fn library_not_defined_after_a_missing_api_is_a_cascade() {
    let list = list();
    let lines = [
        "Uncaught in script: not a function (near \"d.getElementsByTagName\") at https://cdn.test/jq.js:3:1",
        "Uncaught in script: $ is not defined at https://a.test/#inline-0:1:301",
        "Uncaught in timer callback: ReferenceError: jQuery is not defined at https://a.test/x.js:1:2",
    ];
    let verdict = classify_lines(&lines, &list);
    assert_eq!(verdict.kinds(), "missing-api:1,page-bug:0,cascade:2");
    // Without an earlier missing-api the same line is a page bug.
    let verdict = classify_lines(&lines[1..], &list);
    assert_eq!(verdict.kinds(), "missing-api:0,page-bug:2,cascade:0");
    assert_eq!(verdict.first_missing_api, None);
}

#[test]
fn of_undefined_and_unknown_callees_are_page_bugs() {
    let list = list();
    let lines = [
        "Uncaught in script: TypeError: cannot read property 'match' of undefined at https://w.test/load.php:1:20",
        "Uncaught in script: not a function (near \"e.join\") at https://d.test/s.js:2:3",
    ];
    let verdict = classify_lines(&lines, &list);
    assert_eq!(verdict.kinds(), "missing-api:0,page-bug:2,cascade:0");
    assert_eq!(
        verdict.first_error.as_deref(),
        Some("TypeError: cannot read property 'match' of undefined")
    );
    assert!(verdict.first_missing_api.is_none());
}

#[test]
fn repeated_lines_count_once_and_other_levels_are_ignored() {
    let list = list();
    let same = "Uncaught in script: TypeError: cannot read property 'src' of null at https://b.test/t.js:1:9";
    let lines = [same, "console error from the page", same, same];
    let verdict = classify_lines(&lines, &list);
    assert_eq!(verdict.uncaught_dedup, 1);
    assert_eq!(verdict.page_bug, 1);
    // Same message, another place: a separate line.
    let elsewhere = same.replace(":1:9", ":7:9");
    let verdict = classify_lines(&[same, &elsewhere], &list);
    assert_eq!(verdict.uncaught_dedup, 2);
}

#[test]
fn message_strips_the_context_and_the_location_only() {
    assert_eq!(
        message_of("Uncaught in listener: Error: failed at step 2 at https://a.test/x.js:1:2"),
        "Error: failed at step 2"
    );
    assert_eq!(message_of("Uncaught in script: boom"), "boom");
}
