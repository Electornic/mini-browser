// Where a page script error happened (docs/compat-round3.md 0.5). One
// locator for both paths: top-level script errors (`execute_with_url`) and
// listener / timer / rAF errors (`__mb_report_error`, which hands over
// `err.stack`). Both end up as one line:
//
//     {message} (near "{callee}") at {url}:{line}:{col}
//
// Position and `near` come before the URL so the observation's 200-char cut
// eats the URL first. URLs lose their query and fragment (ad / SDK URLs
// carry tokens — AGENTS.md "URL 로그는 쿼리 제외"), except our own
// `#inline-{n}` label for inline scripts.
//
// `near` is page text (a slice of the script source). It may only ever go
// into the page block's `console:` lines, never a trusted `note:`.

/// The first frame of a QuickJS stack that points into a page script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// The script label the source was compiled under (`EvalOptions::filename`),
    /// or `<eval>` when the error only has frames from `eval` / `new Function`
    /// / browser bootstrap code (QuickJS labels those `<input>`).
    pub url: String,
    pub line: u32,
    pub col: u32,
    pub func: Option<String>,
}

const EVAL_LABEL: &str = "<eval>";

/// Longest `near` excerpt; minified bundles can chain long member paths.
const MAX_NEAR_CHARS: usize = 60;

/// Parse a QuickJS `err.stack` (`    at f (https://x/a.js:1:9)` per line).
/// Frames without a position (`(native)`, `(missing)`) are skipped, and so
/// are `<input>` frames when a page frame follows — those are our own
/// bootstrap shims (or `eval` code) sitting on top of the page code that
/// called them, and the page position is what helps.
pub fn locate(stack: &str) -> Option<Location> {
    let mut eval_frame = None;
    for frame in stack.lines() {
        let Some(location) = parse_frame(frame) else {
            continue;
        };
        if location.url.starts_with('<') {
            eval_frame.get_or_insert(Location {
                url: EVAL_LABEL.to_string(),
                ..location
            });
            continue;
        }
        return Some(location);
    }
    eval_frame
}

fn parse_frame(frame: &str) -> Option<Location> {
    let rest = frame.trim().strip_prefix("at ")?;
    let (func, position) = match rest.strip_suffix(')').and_then(|r| r.split_once(" (")) {
        Some((func, position)) => (Some(func.to_string()), position),
        None => (None, rest),
    };
    let mut parts = position.rsplitn(3, ':');
    let col = parts.next()?.parse().ok()?;
    let line = parts.next()?.parse().ok()?;
    let url = parts.next()?.to_string();
    if url.is_empty() {
        return None;
    }
    Some(Location {
        url,
        line,
        col,
        func,
    })
}

/// A script URL as we show it: no query, no fragment — except the
/// `#inline-{n}` label that names an inline script.
pub fn display_url(url: &str) -> String {
    let (base, fragment) = match url.split_once('#') {
        Some((base, fragment)) => (base, Some(fragment)),
        None => (url, None),
    };
    let base = base.split_once('?').map_or(base, |(base, _)| base);
    match fragment {
        Some(fragment) if is_inline_label(fragment) => format!("{base}#{fragment}"),
        _ => base.to_string(),
    }
}

/// `inline-{n}` — the fragment `collect_script_sources` gives inline scripts.
pub fn is_inline_label(fragment: &str) -> bool {
    fragment
        .strip_prefix("inline-")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The callee of a failed call, cut from the source around the error's
/// position: QuickJS says only `not a function`, never what wasn't one.
/// Its column lands on different tokens depending on the call shape —
/// the `.` before the property (`document.nope()`), the space before a
/// bare name (`; foo()`), or just past `(` (`foo( 2 )`, the case the
/// quickjs-ng source suggests) — so try them in that order:
///   1. right after `(` (skipping spaces back) → the expression before it;
///   2. a member/identifier run starting here that is followed by `(`;
///   3. the member/identifier run around the position.
pub fn near(source: &str, line: u32, col: u32) -> Option<String> {
    let text = source.lines().nth(line.checked_sub(1)? as usize)?;
    let chars: Vec<char> = text.chars().collect();
    let at = (col.checked_sub(1)? as usize).min(chars.len());

    let mut back = at;
    while back > 0 && chars[back - 1].is_whitespace() {
        back -= 1;
    }
    let end = if back > 0 && chars[back - 1] == '(' {
        Some(back - 1)
    } else {
        callee_end_from(&chars, at)
    };
    let expression = match end {
        Some(end) => expression_ending_at(&chars, end),
        None => {
            let mut end = at;
            while end < chars.len() && is_member_char(chars[end]) {
                end += 1;
            }
            expression_ending_at(&chars, end)
        }
    };
    let expression = expression.trim_matches('.');
    if !expression
        .chars()
        .any(|c| c.is_alphabetic() || c == '_' || c == '$')
    {
        return None;
    }
    let skip = expression.chars().count().saturating_sub(MAX_NEAR_CHARS);
    Some(expression.chars().skip(skip).collect())
}

// From `at`, skip spaces and a member/identifier run; if a `(` follows
// (after optional spaces) the callee ends where the run ended.
fn callee_end_from(chars: &[char], at: usize) -> Option<usize> {
    let mut i = at;
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    let run_start = i;
    while i < chars.len() && is_member_char(chars[i]) {
        i += 1;
    }
    let run_end = i;
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    (run_end > run_start && chars.get(i) == Some(&'(')).then_some(run_end)
}

fn expression_ending_at(chars: &[char], end: usize) -> String {
    let mut end = end.min(chars.len());
    while end > 0 && chars[end - 1].is_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && is_member_char(chars[start - 1]) {
        start -= 1;
    }
    chars[start..end].iter().collect()
}

fn is_member_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$' || c == '.'
}

/// Whether `near` helps for this message: QuickJS names the property in
/// `cannot read property` and the variable in `is not defined`, but not
/// the callee in `not a function` / `not a constructor`.
pub fn wants_near(message: &str) -> bool {
    message.contains("not a function") || message.contains("not a constructor")
}

/// The one-line form: `{message} (near "x") at {url}:{line}:{col}`.
pub fn describe(message: &str, location: Option<&Location>, near: Option<&str>) -> String {
    let mut out = message.to_string();
    if let Some(near) = near {
        out.push_str(&format!(" (near \"{near}\")"));
    }
    match location {
        Some(location) if location.url == EVAL_LABEL => out.push_str(" at <eval>"),
        Some(location) => out.push_str(&format!(
            " at {}:{}:{}",
            display_url(&location.url),
            location.line,
            location.col
        )),
        None => {}
    }
    out
}

/// Locate `stack`, cut `near` from `source` when it helps, and describe.
/// `source` is looked up by the caller from the frame's script label.
pub fn describe_error<'s>(
    message: &str,
    stack: Option<&str>,
    source_for: impl FnOnce(&str) -> Option<&'s str>,
) -> String {
    let location = stack.and_then(locate);
    let near = location
        .as_ref()
        .filter(|_| wants_near(message))
        .and_then(|l| near(source_for(&l.url)?, l.line, l.col));
    describe(message, location.as_ref(), near.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_caught_error_parses_first_frame() {
        // Named function, anonymous, and the top-level `<eval>` frame.
        let named = locate(
            "    at f (https://a.example/x.js:3:14)\n    at <eval> (https://a.example/x.js:5:1)\n",
        );
        assert_eq!(
            named,
            Some(Location {
                url: "https://a.example/x.js".into(),
                line: 3,
                col: 14,
                func: Some("f".into()),
            })
        );
        let anonymous =
            locate("    at <anonymous> (https://a.example/x.js:1:25)\n    at forEach (native)\n");
        assert_eq!(anonymous.unwrap().col, 25);
        // Frames without a position are skipped.
        let native_first = locate("    at forEach (native)\n    at <eval> (p.html#inline-0:2:7)\n");
        assert_eq!(native_first.unwrap().url, "p.html#inline-0");
        // A bare `at url:line:col` frame (no function name).
        let bare = locate("    at https://a.example/x.js:4:2\n").unwrap();
        assert_eq!((bare.line, bare.col, bare.func), (4, 2, None));
        assert_eq!(locate("    at forEach (native)\n"), None);
        assert_eq!(locate(""), None);
    }

    #[test]
    fn eval_frames_defer_to_the_page_frame_below_them() {
        let stack = "    at <eval> (<input>:1:1)\n    at <eval> (https://a.example/x.js:1:1)\n";
        assert_eq!(locate(stack).unwrap().url, "https://a.example/x.js");
        let only_eval = locate("    at anonymous (<input>:3:1)\n").unwrap();
        assert_eq!(only_eval.url, "<eval>");
        assert_eq!(
            describe("zz is not defined", Some(&only_eval), None),
            "zz is not defined at <eval>"
        );
    }

    #[test]
    fn error_location_strips_query() {
        assert_eq!(
            display_url("https://ad.example/sdk.js?token=abc"),
            "https://ad.example/sdk.js"
        );
        assert_eq!(
            display_url("https://ad.example/sdk.js#frag"),
            "https://ad.example/sdk.js"
        );
        assert_eq!(
            display_url("https://a.example/p?q=1#inline-3"),
            "https://a.example/p#inline-3"
        );
        assert_eq!(
            display_url("https://a.example/p#inline-x"),
            "https://a.example/p"
        );
        let line = describe_error(
            "not a function",
            Some("    at <eval> (https://ad.example/sdk.js?token=abc:1:9)\n"),
            |_| Some("document.nope()"),
        );
        assert_eq!(
            line,
            "not a function (near \"document.nope\") at https://ad.example/sdk.js:1:9"
        );
        assert!(!line.contains("token"));
    }

    // Columns as quickjs-ng (rquickjs 0.11) reports them for `not a function`.
    #[test]
    fn near_finds_the_callee_for_each_column_shape() {
        // Column on the `.` before the property.
        assert_eq!(
            near("document.nope()", 1, 9).as_deref(),
            Some("document.nope")
        );
        assert_eq!(
            near("console.debug('x')", 1, 8).as_deref(),
            Some("console.debug")
        );
        assert_eq!(
            near("  document.getElementsByTagName('head')", 1, 11).as_deref(),
            Some("document.getElementsByTagName")
        );
        assert_eq!(
            near("var a = {b:{}}; a.b.c(1)", 1, 20).as_deref(),
            Some("a.b.c")
        );
        // Column on the space before a bare name.
        assert_eq!(near("var foo = 1; foo()", 1, 13).as_deref(), Some("foo"));
        // Column after the property name, before a spaced `(`.
        assert_eq!(
            near("var a = {b:{}}; a.b.c (1)", 1, 22).as_deref(),
            Some("a.b.c")
        );
        // Parenthesised callee: no `(` follows, take the run around.
        assert_eq!(
            near("(0, document.qq)()", 1, 13).as_deref(),
            Some("document.qq")
        );
        // Inside a function body the column lands on the property name.
        assert_eq!(
            near("  window.nope();", 1, 10).as_deref(),
            Some("window.nope")
        );
        // Later lines.
        assert_eq!(near("var x;\n  x.y.z();", 2, 6).as_deref(), Some("x.y.z"));
    }

    #[test]
    fn near_skips_paren_before_call() {
        // A column just past `(` (and spaces) — the shape the quickjs-ng
        // source describes — still names the callee, not the argument.
        assert_eq!(near("var foo = 1; foo( 2 )", 1, 18).as_deref(), Some("foo"));
        assert_eq!(near("win.open(url)", 1, 10).as_deref(), Some("win.open"));
    }

    #[test]
    fn near_gives_up_cleanly() {
        assert_eq!(near("f()", 0, 1), None);
        assert_eq!(near("f()", 9, 1), None);
        assert_eq!(near("(1)(2)", 1, 4), None);
        // Multibyte text around the position never splits a character.
        assert_eq!(near("한글.없음()", 1, 3).as_deref(), Some("한글.없음"));
        // Long minified chains keep their tail.
        let long = format!("{}.f()", "a.".repeat(100).trim_end_matches('.'));
        let got = near(&long, 1, 200).unwrap();
        assert!(
            got.ends_with(".f") && got.chars().count() <= MAX_NEAR_CHARS,
            "{got}"
        );
    }

    #[test]
    fn near_is_only_added_for_call_errors() {
        let line = describe_error(
            "cannot read property 'y' of undefined",
            Some("    at <eval> (https://a.example/x.js:1:13)\n"),
            |_| Some("var g = {}; g.x.y()"),
        );
        assert_eq!(
            line,
            "cannot read property 'y' of undefined at https://a.example/x.js:1:13"
        );
        let no_stack = describe_error("not a function", None, |_| None);
        assert_eq!(no_stack, "not a function");
    }
}
