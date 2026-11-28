// Script-error classification (docs/compat-round4.md step 0 P4): the
// `Uncaught` lines of a saved `console.txt`, judged by the frozen rules
// against `scripts/compat-js-apis.tsv`. Post-processing only — the probe
// runs it on the console it just wrote, `compat_probe --classify DIR` on an
// old label's (no engine rerun when the list changes).
//
//   missing-api  `not a function (near "a.b")` → b, or `X is not defined`
//                → X, is an `api` in the list
//   cascade      `X is not defined`, X a `cascade` name, after a
//                missing-api line in the same run
//   page-bug     everything else (`… of undefined` included — the cookie
//                cases are judged by hand, appendix B)
//
// Lines are deduplicated by their whole text (message + location) first.

use std::collections::BTreeSet;

/// The list file, parsed.
#[derive(Debug, Default)]
pub struct ApiList {
    apis: BTreeSet<String>,
    cascades: BTreeSet<String>,
}

impl ApiList {
    /// `name<TAB>kind<TAB>note` lines; `#` comments and blanks skipped.
    pub fn parse(tsv: &str) -> Self {
        let mut list = Self::default();
        for line in tsv.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let mut fields = line.split('\t');
            let (Some(name), Some(kind)) = (fields.next(), fields.next()) else {
                continue;
            };
            match kind.trim() {
                "api" => list.apis.insert(name.trim().to_string()),
                "cascade" => list.cascades.insert(name.trim().to_string()),
                _ => false,
            };
        }
        list
    }

    pub fn len(&self) -> usize {
        self.apis.len() + self.cascades.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    MissingApi(String),
    Cascade(String),
    PageBug,
}

/// The P4 columns for one console.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JsVerdict {
    /// The first `Uncaught` line's message: context prefix and location
    /// removed, 60 characters.
    pub first_error: Option<String>,
    /// The first line's API when it is a missing-api.
    pub first_missing_api: Option<String>,
    /// Every missing-api name, first-seen order, once each.
    pub any_missing_api: Vec<String>,
    pub missing_api: usize,
    pub page_bug: usize,
    pub cascade: usize,
    /// `Uncaught` lines after deduplication.
    pub uncaught_dedup: usize,
}

impl JsVerdict {
    /// `missing-api:N,page-bug:N,cascade:N`.
    pub fn kinds(&self) -> String {
        format!(
            "missing-api:{},page-bug:{},cascade:{}",
            self.missing_api, self.page_bug, self.cascade
        )
    }
}

/// `console.txt` (`level<TAB>text` per line, as the probe writes it).
pub fn classify_console(console: &str, list: &ApiList) -> JsVerdict {
    let lines: Vec<&str> = console
        .lines()
        .filter_map(|line| line.split_once('\t').map(|(_, text)| text))
        .collect();
    classify_lines(&lines, list)
}

/// Console texts in console order (only `Uncaught …` ones count).
pub fn classify_lines(lines: &[&str], list: &ApiList) -> JsVerdict {
    let mut seen = BTreeSet::new();
    let mut verdict = JsVerdict::default();
    for text in lines.iter().filter(|t| t.starts_with("Uncaught")) {
        if !seen.insert(*text) {
            continue;
        }
        verdict.uncaught_dedup += 1;
        let message = message_of(text);
        let kind = kind_of(&message, list, verdict.missing_api > 0);
        if verdict.first_error.is_none() {
            verdict.first_error = Some(clip(&message, 60));
            if let ErrorKind::MissingApi(name) = &kind {
                verdict.first_missing_api = Some(name.clone());
            }
        }
        match kind {
            ErrorKind::MissingApi(name) => {
                verdict.missing_api += 1;
                if !verdict.any_missing_api.contains(&name) {
                    verdict.any_missing_api.push(name);
                }
            }
            ErrorKind::Cascade(_) => verdict.cascade += 1,
            ErrorKind::PageBug => verdict.page_bug += 1,
        }
    }
    verdict
}

/// `Uncaught in script: TypeError: x (near "a") at URL:1:2` → `TypeError: x (near "a")`.
pub fn message_of(text: &str) -> String {
    let body = match text.strip_prefix("Uncaught") {
        Some(rest) => match rest.split_once(": ") {
            Some((context, message))
                if context.trim_start().starts_with("in ") || context.is_empty() =>
            {
                message
            }
            _ => rest.trim_start_matches(':').trim_start(),
        },
        None => text,
    };
    // The location is the last ` at ` (URLs have no spaces; messages may
    // contain " at " themselves).
    match body.rsplit_once(" at ") {
        Some((message, location)) if !location.contains(' ') && location.contains(':') => {
            message.to_string()
        }
        _ => body.to_string(),
    }
}

pub fn kind_of(message: &str, list: &ApiList, after_missing_api: bool) -> ErrorKind {
    if message.contains("not a function") || message.contains("not a constructor") {
        if let Some(callee) = near(message) {
            let name = callee
                .trim_start_matches("new ")
                .rsplit('.')
                .next()
                .unwrap_or(callee)
                .trim();
            if list.apis.contains(name) {
                return ErrorKind::MissingApi(name.to_string());
            }
        }
        return ErrorKind::PageBug;
    }
    if let Some(name) = not_defined(message) {
        if list.apis.contains(name) {
            return ErrorKind::MissingApi(name.to_string());
        }
        if after_missing_api && list.cascades.contains(name) {
            return ErrorKind::Cascade(name.to_string());
        }
    }
    ErrorKind::PageBug
}

/// The `near "…"` callee.
fn near(message: &str) -> Option<&str> {
    let start = message.find("(near \"")? + "(near \"".len();
    let rest = &message[start..];
    Some(&rest[..rest.find('"')?])
}

/// `X` of `[ReferenceError: ]X is not defined`.
fn not_defined(message: &str) -> Option<&str> {
    let head = message.split(" is not defined").next()?;
    if head.len() == message.len() {
        return None;
    }
    let name = head.rsplit([' ', '\'', '"']).next()?;
    (!name.is_empty()).then_some(name)
}

fn clip(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((cut, _)) => text[..cut].to_string(),
        None => text.to_string(),
    }
}
