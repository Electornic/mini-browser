// Real-site compatibility probe (docs/compat-survey.md, docs/compat-round3.md 0.2).
//
//   MB_NET_CACHE=$PWD/compat-cache/net cargo run --release -p mb-agent \
//       --example compat_probe -- URL OUT_DIR ["role:name;role:name…"]
//
// Normally driven by scripts/compat-corpus.sh. Loads URL the way an agent
// would (AgentBrowser::navigate, system fonts), then writes to OUT_DIR:
//   observation.txt  what the agent sees (viewport observation + whole page)
//   elements.txt     one `role<TAB>name` line per element (whole page, no
//                    ids) — diff two runs to see what appeared/disappeared
//   console.txt      every console message of the load
//   css.txt          what the stylesheet parser dropped, by reason and
//                    origin (ours / browser-too), and parsed properties
//                    nothing downstream reads
//   layout.txt       layout anomalies (text squeezed into slivers, big empty
//                    boxes, boxes pushed off-page)
//   summary.txt      the headline numbers
//   summary.tsv      the same, machine-readable: a header line + one row
//
// The optional third argument is the site's expected controls from
// scripts/compat-sites.tsv; each `role:substring` must match some element
// (role exactly, name case-insensitively contains the substring;
// `role:a|b` accepts either, for sites that pick a language by IP).
// `compat_probe --header` prints the summary.tsv header and exits.
//
// Always run it with MB_NET_CACHE (see crates/mb-runtime/src/net/cache.rs)
// so the site is fetched once, and keep OUT_DIR out of the repo: it holds
// third-party text.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use mb_agent::{AgentBrowser, AgentConfig, ClockMode, Observation};
use mb_runtime::{
    css::{self, DropReason, Origin, Value, properties},
    dom::{Document, NodeId, NodeType},
    layout::{BoxType, LayoutBox},
    net,
};

/// Properties something downstream of the cascade actually reads (layout,
/// paint, the agent index). A parsed declaration outside this list is
/// accepted and then ignored. Kept by hand from `styled.value("…")` call
/// sites; inherited lookups (`color`, `font-*`…) included.
const CONSUMED: &[&str] = &[
    "align-content",
    "align-items",
    "align-self",
    "background-color",
    "background-image",
    "background-position-x",
    "background-position-y",
    "border-bottom",
    "border-bottom-left-radius",
    "border-bottom-right-radius",
    "border-color",
    "border-left",
    "border-right",
    "border-spacing",
    "border-style",
    "border-top",
    "border-top-left-radius",
    "border-top-right-radius",
    "border-width",
    "bottom",
    "box-shadow",
    "box-sizing",
    "clear",
    "color",
    "column-gap",
    "display",
    "flex-basis",
    "flex-direction",
    "flex-grow",
    "flex-shrink",
    "flex-wrap",
    "float",
    "font-family",
    "font-size",
    "font-weight",
    "gap",
    "grid-area",
    "grid-auto-flow",
    "grid-column",
    "grid-row",
    "grid-template-areas",
    "grid-template-columns",
    "grid-template-rows",
    "height",
    "justify-content",
    "left",
    "line-height",
    "margin-bottom",
    "margin-left",
    "margin-right",
    "margin-top",
    "max-height",
    "max-width",
    "min-height",
    "min-width",
    "opacity",
    "overflow",
    "overflow-x",
    "overflow-y",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "padding-top",
    "position",
    "right",
    "row-gap",
    "text-align",
    "text-decoration",
    "text-shadow",
    "top",
    "transform",
    "vertical-align",
    "white-space",
    "width",
    "z-index",
];

fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: compat_probe URL OUT_DIR [\"role:name;…\"]";
    let url = args.next().expect(usage);
    if url == "--header" {
        let mut row = Row::default();
        push_columns(&mut row, None);
        print!("{}", row.render().lines().next().unwrap_or_default());
        println!();
        return;
    }
    let out = PathBuf::from(args.next().expect(usage));
    let expected = parse_expected(&args.next().unwrap_or_default());
    fs::create_dir_all(&out).expect("create OUT_DIR");

    let mut browser = AgentBrowser::new(AgentConfig {
        system_fonts: true,
        clock: ClockMode::Virtual,
        settle_timeout: Duration::from_secs(20),
        ..AgentConfig::default()
    });
    let started = Instant::now();
    let viewport = match browser.navigate(&url) {
        Ok(observation) => observation,
        Err(error) => {
            // Not a page load failure (those show the error page): the URL
            // or the agent refused. Still a nav-fail for the corpus.
            eprintln!("navigate failed: {error:?}");
            let mut row = Row::default();
            push_columns(&mut row, None);
            row.values[0] = one_line(&url);
            row.values[1] = "nav-fail".into();
            write(&out, "summary.tsv", &row.render());
            std::process::exit(1);
        }
    };
    let load_ms = started.elapsed().as_millis();
    let whole = browser.full_snapshot();
    let mut observation = String::new();
    let _ = writeln!(
        observation,
        "=== viewport observation ===\n{}",
        viewport.to_text()
    );
    let _ = writeln!(observation, "\n=== whole page ===\n{}", whole.to_text());
    write(&out, "observation.txt", &observation);
    write(&out, "elements.txt", &element_lines(&whole));
    let mut console = String::new();
    for message in &viewport.console {
        let _ = writeln!(console, "{}\t{}", message.level, one_line(&message.text));
    }
    write(&out, "console.txt", &console);

    let state = browser.state();
    // A failed document load commits the canned error page, which has no URL.
    let document = state.parsed_document.borrow();
    // Read from the DOM, not the observation: the error page's text is
    // what says why (`http status 403`), whether or not it renders.
    let load_error = state.current_url.is_none().then(|| dom_text(&document));
    let inline_css = inline_style_text(&document);
    let media_ctx = (
        browser.config().viewport_width as f32,
        browser.config().viewport_height as f32,
    );
    let mut css_report = css_drops(&state.stylesheet, &inline_css, media_ctx);
    css_report.counts.media_attr_sheets = media_attr_sheets(&document);
    write(&out, "css.txt", &css_report.text);

    let view = state.current_view().expect("a laid-out view");
    let mut anomalies = Anomalies::default();
    walk_layout(
        &view.layout_root,
        &document,
        state.parsed_stylesheets.len(),
        &mut anomalies,
    );
    let page_width = browser.config().viewport_width as f32;
    write(&out, "layout.txt", &anomalies.render(page_width));

    let cache = net::survey_stats();
    let console_errors = viewport
        .console
        .iter()
        .filter(|m| m.level == "error")
        .count();
    let script_errors = viewport
        .console
        .iter()
        .filter(|m| m.text.starts_with("Uncaught"))
        .count();
    let (found, missing) = match_expected(&expected, &whole);
    let result = match &load_error {
        Some(text) if is_blocked(text) => "blocked",
        Some(_) => "nav-fail",
        None if viewport.settled == Some(false) => "settle-timeout",
        None if !cache.offline_misses.is_empty() => "ok*",
        None => "ok",
    };

    let mut summary = String::new();
    let _ = writeln!(summary, "url: {url}");
    let _ = writeln!(summary, "result: {result} ({load_ms} ms)");
    if let Some(text) = &load_error {
        let _ = writeln!(summary, "load error page: {}", clip(text, 200));
    }
    let _ = writeln!(
        summary,
        "final: {:?}",
        state.current_url.as_ref().map(|u| u.to_string())
    );
    let _ = writeln!(
        summary,
        "css bytes: external {} / inline {}",
        state.stylesheet.len(),
        inline_css.len()
    );
    let _ = writeln!(summary, "{}", css_report.headline);
    let images = image_stats(&document);
    let _ = writeln!(
        summary,
        "images: {} <img> ({} with src, {} lazy-only data-src/srcset, {} data: URIs) / {} decoded total (incl. CSS)",
        images.0,
        images.1,
        images.2,
        images.3,
        state.images.len()
    );
    let _ = writeln!(
        summary,
        "observation: {} elements (whole page), {} in viewport, {} text blocks, console {} ({} errors, {} uncaught)",
        whole.total_elements,
        viewport.elements.len(),
        whole.text.len(),
        viewport.console.len(),
        console_errors,
        script_errors
    );
    for message in viewport
        .console
        .iter()
        .filter(|m| m.level == "error")
        .take(10)
    {
        let _ = writeln!(
            summary,
            "  console.{}: {}",
            message.level,
            clip(&message.text, 200)
        );
    }
    let _ = writeln!(
        summary,
        "expected controls: {}/{} found{}",
        found,
        expected.len(),
        if missing.is_empty() {
            String::new()
        } else {
            format!(", missing {}", missing.join(", "))
        }
    );
    let _ = writeln!(
        summary,
        "survey cache: {} offline misses ({}), {} non-GET refused",
        cache.offline_misses.len(),
        miss_kinds(&cache.offline_misses),
        cache.refused_non_get
    );
    for miss in cache.offline_misses.iter().take(20) {
        let _ = writeln!(summary, "  miss: {}", without_query(miss));
    }
    let _ = writeln!(summary, "layout: {}", anomalies.headline());
    let _ = writeln!(summary, "document height: {}", whole.document_height);
    write(&out, "summary.txt", &summary);
    print!("{summary}");

    let columns = Columns {
        url: &url,
        result,
        load_ms,
        whole: &whole,
        viewport: &viewport,
        css: &css_report.counts,
        slivers: anomalies.slivers.len(),
        script_errors,
        console_errors,
        cache: &cache,
        expected: expected.len(),
        found,
        missing: &missing,
        css_bytes: state.stylesheet.len() + inline_css.len(),
        final_url: state
            .current_url
            .as_ref()
            .map(|u| without_query(&u.to_string()).to_string())
            .unwrap_or_else(|| "-".into()),
    };
    let mut row = Row::default();
    push_columns(&mut row, Some(&columns));
    write(&out, "summary.tsv", &row.render());
}

struct Columns<'a> {
    url: &'a str,
    result: &'a str,
    load_ms: u128,
    whole: &'a Observation,
    viewport: &'a Observation,
    css: &'a CssCounts,
    slivers: usize,
    script_errors: usize,
    console_errors: usize,
    cache: &'a net::SurveyCacheStats,
    expected: usize,
    found: usize,
    missing: &'a [String],
    css_bytes: usize,
    final_url: String,
}

/// The `summary.tsv` columns, in order. `None` = header only (values "-"),
/// so `--header` and a real row can never disagree.
fn push_columns(row: &mut Row, columns: Option<&Columns>) {
    let mut col = |name: &'static str, value: &dyn Fn(&Columns) -> String| {
        row.push(name, &columns.map(value).unwrap_or_default());
    };
    col("url", &|c| c.url.to_string());
    col("result", &|c| c.result.to_string());
    col("load_ms", &|c| c.load_ms.to_string());
    col("elements", &|c| c.whole.total_elements.to_string());
    col("viewport_elements", &|c| {
        c.viewport.elements.len().to_string()
    });
    col("text_blocks", &|c| c.whole.text.len().to_string());
    col("decl_drop_ours", &|c| c.css.decl_ours.to_string());
    col("decl_drop_browser", &|c| c.css.decl_browser.to_string());
    col("rule_drop_ours", &|c| c.css.rule_ours.to_string());
    col("rule_drop_browser", &|c| c.css.rule_browser.to_string());
    col("slivers", &|c| c.slivers.to_string());
    col("script_errors", &|c| c.script_errors.to_string());
    col("console_errors", &|c| c.console_errors.to_string());
    col("offline_miss", &|c| {
        c.cache.offline_misses.len().to_string()
    });
    col("miss_kinds", &|c| miss_kinds(&c.cache.offline_misses));
    col("non_get_refused", &|c| c.cache.refused_non_get.to_string());
    col("controls_found", &|c| format!("{}/{}", c.found, c.expected));
    col("controls_missing", &|c| c.missing.join(";"));
    col("decl_kept", &|c| c.css.decl_kept.to_string());
    col("rule_kept", &|c| c.css.rule_kept.to_string());
    col("keyword_fallback", &|c| c.css.keyword_fallback.to_string());
    col("parse_only", &|c| c.css.parse_only.to_string());
    col("at_skipped_ours", &|c| c.css.at_ours.to_string());
    col("at_skipped_browser", &|c| c.css.at_browser.to_string());
    col("at_skipped_bytes", &|c| c.css.at_skipped_bytes.to_string());
    col("import", &|c| c.css.imports.to_string());
    col("nesting", &|c| c.css.nesting.to_string());
    col("media", &|c| c.css.media.to_string());
    col("media_rules", &|c| c.css.media_rules.to_string());
    col("media_applied", &|c| c.css.media_applied.to_string());
    col("media_wrong", &|c| c.css.media_wrong.to_string());
    col("media_wrong_applied", &|c| {
        c.css.media_wrong_applied.to_string()
    });
    col("media_invalid", &|c| c.css.media_invalid.to_string());
    col("media_attr_sheets", &|c| {
        c.css.media_attr_sheets.to_string()
    });
    col("decl_reasons", &|c| c.css.decl_reasons.clone());
    col("css_bytes", &|c| c.css_bytes.to_string());
    col("document_height", &|c| {
        format!("{:.0}", c.whole.document_height)
    });
    col("final_url", &|c| c.final_url.clone());
}

/// One `summary.tsv` row with its header, column order = push order.
#[derive(Default)]
struct Row {
    names: Vec<&'static str>,
    values: Vec<String>,
}

impl Row {
    fn push(&mut self, name: &'static str, value: &str) {
        self.names.push(name);
        let value = one_line(value);
        self.values
            .push(if value.is_empty() { "-".into() } else { value });
    }

    fn render(&self) -> String {
        format!("{}\n{}\n", self.names.join("\t"), self.values.join("\t"))
    }
}

fn one_line(text: &str) -> String {
    text.replace(['\t', '\n', '\r'], " ")
}

fn without_query(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

/// What a load-error page says about the response: 401/403/429/503 or a
/// challenge are a bot block (recorded, never worked around).
fn is_blocked(error_page_text: &str) -> bool {
    ["401", "403", "429", "503"]
        .iter()
        .any(|code| error_page_text.contains(&format!("http status {code}")))
}

/// Offline misses by kind, from the URL path's extension.
fn miss_kinds(misses: &[String]) -> String {
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for miss in misses {
        let path = without_query(miss.rsplit(' ').next().unwrap_or(miss));
        let ext = path
            .rsplit('/')
            .next()
            .and_then(|file| file.rsplit_once('.'))
            .map(|(_, ext)| ext.to_ascii_lowercase())
            .unwrap_or_default();
        let kind = match ext.as_str() {
            "css" => "css",
            "js" | "mjs" => "js",
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" | "avif" => "img",
            "woff" | "woff2" | "ttf" | "otf" => "font",
            _ => "other",
        };
        *kinds.entry(kind).or_default() += 1;
    }
    kinds
        .iter()
        .map(|(kind, count)| format!("{kind}:{count}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// `role:substring;role:a|b` → pairs (blank items skipped).
fn parse_expected(spec: &str) -> Vec<(String, String)> {
    spec.split(';')
        .filter_map(|item| item.trim().split_once(':'))
        .map(|(role, name)| (role.trim().to_ascii_lowercase(), name.trim().to_lowercase()))
        .filter(|(role, name)| !role.is_empty() && !name.is_empty())
        .collect()
}

/// (found count, missing `role:name` labels).
fn match_expected(expected: &[(String, String)], whole: &Observation) -> (usize, Vec<String>) {
    let mut missing = Vec::new();
    for (role, name) in expected {
        let hit = whole.elements.iter().any(|e| {
            let element_name = e.name.to_lowercase();
            e.role.as_str() == role
                && name
                    .split('|')
                    .any(|alternative| element_name.contains(alternative.trim()))
        });
        if !hit {
            missing.push(format!("{role}:{name}"));
        }
    }
    (expected.len() - missing.len(), missing)
}

/// `role<TAB>name` per element in document order, without ids (ids shift
/// whenever an earlier element appears), so two runs diff cleanly.
fn element_lines(whole: &Observation) -> String {
    let mut text = String::new();
    for element in &whole.elements {
        let _ = writeln!(
            text,
            "{}\t{}",
            element.role.as_str(),
            clip(&one_line(&element.name), 120)
        );
    }
    text
}

fn write(dir: &Path, name: &str, text: &str) {
    fs::write(dir.join(name), text).expect("write report");
}

fn clip(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_string(),
    }
}

/// (`<img>` count, with `src`, only `data-src`/`srcset`, `data:` src).
fn image_stats(document: &Document) -> (usize, usize, usize, usize) {
    let mut stats = (0, 0, 0, 0);
    let mut stack: Vec<NodeId> = document.roots().to_vec();
    while let Some(id) = stack.pop() {
        let Some(node) = document.get(id) else {
            continue;
        };
        if let NodeType::Element(element) = &node.node_type
            && element.tag_name == "img"
        {
            stats.0 += 1;
            let src = element
                .attributes
                .get("src")
                .map(|s| s.trim())
                .unwrap_or("");
            if !src.is_empty() {
                stats.1 += 1;
                if src.starts_with("data:") {
                    stats.3 += 1;
                }
            } else if element.attributes.contains_key("data-src")
                || element.attributes.contains_key("srcset")
            {
                stats.2 += 1;
            }
        }
        stack.extend(node.children.iter().copied());
    }
    stats
}

/// Every text node, in document order, joined by spaces.
fn dom_text(document: &Document) -> String {
    let mut parts = Vec::new();
    let mut stack: Vec<NodeId> = document.roots().iter().rev().copied().collect();
    while let Some(id) = stack.pop() {
        let Some(node) = document.get(id) else {
            continue;
        };
        if let NodeType::Text(text) = &node.node_type
            && !text.trim().is_empty()
        {
            parts.push(text.trim().to_string());
        }
        stack.extend(node.children.iter().rev().copied());
    }
    parts.join(" ")
}

fn inline_style_text(document: &Document) -> String {
    let mut css = String::new();
    let mut stack: Vec<NodeId> = document.roots().iter().rev().copied().collect();
    while let Some(id) = stack.pop() {
        let Some(node) = document.get(id) else {
            continue;
        };
        if let NodeType::Element(element) = &node.node_type
            && element.tag_name.eq_ignore_ascii_case("style")
        {
            // `<style media=…>` as a block, the way the engine joins it
            // (docs/compat-round3.md 2.1) — so the counts match what applies.
            let media = sheet_media(element.attributes.get("media"));
            if let Some(media) = media {
                let _ = writeln!(css, "@media {media} {{");
            }
            for child in &node.children {
                if let Some(NodeType::Text(text)) = document.get(*child).map(|n| &n.node_type) {
                    css.push_str(text);
                    css.push('\n');
                }
            }
            if media.is_some() {
                css.push_str("}\n");
            }
        }
        stack.extend(node.children.iter().rev().copied());
    }
    css
}

/// A `media` attribute that limits a sheet (`None` = applies everywhere).
fn sheet_media(media: Option<&String>) -> Option<&str> {
    media
        .map(|m| m.trim())
        .filter(|m| !m.is_empty() && !m.eq_ignore_ascii_case("all"))
}

/// `<link rel=stylesheet>` and `<style>` elements with a limiting `media`.
fn media_attr_sheets(document: &Document) -> usize {
    let mut count = 0;
    let mut stack: Vec<NodeId> = document.roots().iter().rev().copied().collect();
    while let Some(id) = stack.pop() {
        let Some(node) = document.get(id) else {
            continue;
        };
        if let NodeType::Element(element) = &node.node_type {
            let tag = element.tag_name.to_ascii_lowercase();
            let sheet = tag == "style"
                || (tag == "link"
                    && element
                        .attributes
                        .get("rel")
                        .is_some_and(|rel| rel.eq_ignore_ascii_case("stylesheet")));
            if sheet && sheet_media(element.attributes.get("media")).is_some() {
                count += 1;
            }
        }
        stack.extend(node.children.iter().rev().copied());
    }
    count
}

// ---------------------------------------------------------------------------
// CSS drops
// ---------------------------------------------------------------------------

struct CssReport {
    headline: String,
    text: String,
    counts: CssCounts,
}

/// The `summary.tsv` CSS columns (classification: docs/compat-round3.md
/// "0b 분류표", reasons and origins from `css::DropReason`).
#[derive(Default)]
struct CssCounts {
    rule_kept: usize,
    decl_kept: usize,
    decl_ours: usize,
    decl_browser: usize,
    rule_ours: usize,
    rule_browser: usize,
    at_ours: usize,
    at_browser: usize,
    at_skipped_bytes: usize,
    /// Kept declarations of a property with a modelled value kind whose
    /// value is a `Keyword` that kind doesn't accept (`width: 100vw`).
    keyword_fallback: usize,
    /// Kept declarations nothing downstream reads (see `CONSUMED`).
    parse_only: usize,
    imports: usize,
    /// Nested rules inside blocks + top-level selectors with `&`; kept out
    /// of the declaration counts.
    nesting: usize,
    /// `@media` blocks (nested ones included).
    media: usize,
    /// Rules inside `@media` blocks / of those, the ones the engine applies
    /// at the probe's viewport.
    media_rules: usize,
    media_applied: usize,
    /// Rules inside a block `media_kind` says must not apply at 1280x800
    /// light (`… (wrong…)`, the block or an enclosing one) / of those, the
    /// ones the engine applies anyway. The goal is `media_wrong_applied` 0.
    media_wrong: usize,
    media_wrong_applied: usize,
    /// Comma-separated queries the engine couldn't parse (`not all`).
    media_invalid: usize,
    /// `<link rel=stylesheet>` / `<style>` with a limiting `media` attribute.
    media_attr_sheets: usize,
    /// `code:count` of every declaration drop reason.
    decl_reasons: String,
}

fn css_drops(external: &str, inline: &str, (width, height): (f32, f32)) -> CssReport {
    let mut text = String::new();
    let mut c = CssCounts::default();
    let mut ignored: BTreeMap<String, usize> = BTreeMap::new();
    let mut fallbacks: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let mut dropped_props: BTreeMap<(Origin, DropReason, String), (usize, String)> =
        BTreeMap::new();
    let mut decl_reasons: BTreeMap<DropReason, usize> = BTreeMap::new();
    let mut rule_causes: BTreeMap<(Origin, &'static str), (usize, String)> = BTreeMap::new();
    let mut at_rules: BTreeMap<String, (usize, bool, usize)> = BTreeMap::new();
    // kind → (blocks, rules, rules applied)
    let mut media_kinds: BTreeMap<&'static str, (usize, usize, usize)> = BTreeMap::new();

    for source in [external, inline] {
        let (sheet, report) = css::parse_with_report(source);
        c.rule_kept += sheet.rules.len();
        for rule in &sheet.rules {
            for decl in &rule.declarations {
                c.decl_kept += 1;
                if !decl.name.starts_with("--") && !CONSUMED.contains(&decl.name.as_str()) {
                    c.parse_only += 1;
                    *ignored.entry(decl.name.clone()).or_default() += 1;
                }
                if let (Some(kind), Value::Keyword(keyword)) =
                    (properties::lookup(&decl.name), &decl.value)
                    && kind.is_typed()
                    && !kind.accepts_keyword(keyword)
                {
                    c.keyword_fallback += 1;
                    let entry = fallbacks
                        .entry(decl.name.clone())
                        .or_insert((0, keyword.clone()));
                    entry.0 += 1;
                }
            }
        }
        for decl in &report.dropped_declarations {
            *decl_reasons.entry(decl.reason).or_default() += 1;
            if decl.reason == DropReason::NestedRule {
                c.nesting += 1;
            } else if decl.reason.origin() == Origin::Ours {
                c.decl_ours += 1;
            } else {
                c.decl_browser += 1;
            }
            let entry = dropped_props
                .entry((decl.reason.origin(), decl.reason, decl.property.clone()))
                .or_insert((0, decl.text.clone()));
            entry.0 += 1;
        }
        for rule in &report.dropped_rules {
            if rule.selector.contains('&') {
                c.nesting += 1;
            }
            match rule.reason.origin() {
                Origin::Ours => c.rule_ours += 1,
                Origin::BrowserToo => c.rule_browser += 1,
            }
            let entry = rule_causes
                .entry((rule.reason.origin(), selector_cause(&rule.selector)))
                .or_insert((0, rule.selector.clone()));
            entry.0 += 1;
        }
        count_media(&report.at_rules, (width, height), &mut c, &mut media_kinds);
        for at_rule in &report.at_rules {
            if at_rule.name == "import" {
                c.imports += 1;
            }
            match at_rule.drop_reason().map(DropReason::origin) {
                Some(Origin::Ours) => c.at_ours += 1,
                Some(Origin::BrowserToo) => c.at_browser += 1,
                None => {}
            }
            c.at_skipped_bytes += at_rule.skipped_bytes;
            let entry = at_rules
                .entry(at_rule.name.clone())
                .or_insert((0, at_rule.kept, 0));
            entry.0 += 1;
            entry.2 += at_rule.skipped_bytes;
        }
    }
    c.decl_reasons = decl_reasons
        .iter()
        .map(|(reason, count)| format!("{}:{count}", reason.code()))
        .collect::<Vec<_>>()
        .join(",");

    let headline = format!(
        "css: {} rules kept, rules dropped {} ours / {} browser-too, declarations dropped {} ours / {} browser-too ({}), {} declarations kept ({} keyword fallbacks, {} parse-only), {} nested rules, {} @import, {} @media ({} rules, {} applied; {} should not apply, {} of them applied; {} invalid queries), at-rules skipped {} ours / {} browser-too ({} bytes)",
        c.rule_kept,
        c.rule_ours,
        c.rule_browser,
        c.decl_ours,
        c.decl_browser,
        c.decl_reasons,
        c.decl_kept,
        c.keyword_fallback,
        c.parse_only,
        c.nesting,
        c.imports,
        c.media,
        c.media_rules,
        c.media_applied,
        c.media_wrong,
        c.media_wrong_applied,
        c.media_invalid,
        c.at_ours,
        c.at_browser,
        c.at_skipped_bytes
    );
    let _ = writeln!(text, "{headline}\n");

    let _ = writeln!(
        text,
        "## dropped declarations by origin / reason / property (count, sample)"
    );
    let mut props: Vec<_> = dropped_props.into_iter().collect();
    props.sort_by(|a, b| a.0.0.cmp(&b.0.0).then(b.1.0.cmp(&a.1.0)));
    for ((origin, reason, name), (count, sample)) in props.iter().take(80) {
        let _ = writeln!(
            text,
            "{count:6}  {:11} {:16} {name:28} {}",
            origin_code(*origin),
            reason.code(),
            clip(sample, 80)
        );
    }

    let _ = writeln!(
        text,
        "\n## dropped rules by origin / selector cause (count, sample)"
    );
    let mut causes: Vec<_> = rule_causes.into_iter().collect();
    causes.sort_by(|a, b| a.0.0.cmp(&b.0.0).then(b.1.0.cmp(&a.1.0)));
    for ((origin, cause), (count, sample)) in causes {
        let _ = writeln!(
            text,
            "{count:6}  {:11} {cause:28} {}",
            origin_code(origin),
            clip(&sample, 80)
        );
    }

    let _ = writeln!(text, "\n## at-rules (count, body kept?, skipped bytes)");
    for (name, (count, kept, bytes)) in &at_rules {
        let _ = writeln!(
            text,
            "{count:6}  @{name} {} {bytes}",
            if *kept { "kept" } else { "SKIPPED" }
        );
    }

    let _ = writeln!(
        text,
        "\n## @media conditions (blocks, rules, rules applied by the engine at {width}x{height}; 'wrong at 1280x800 light' = should NOT apply)"
    );
    for (kind, (blocks, rules, applied)) in &media_kinds {
        let _ = writeln!(text, "{blocks:6} {rules:6} {applied:6}  {kind}");
    }

    let _ = writeln!(
        text,
        "\n## keyword fallbacks: typed property kept a Keyword it doesn't accept (count, sample)"
    );
    let mut fallbacks: Vec<_> = fallbacks.into_iter().collect();
    fallbacks.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    for (name, (count, sample)) in fallbacks.iter().take(40) {
        let _ = writeln!(text, "{count:6}  {name:28} {}", clip(sample, 60));
    }

    let _ = writeln!(text, "\n## parsed but never read downstream (count)");
    let mut ignored: Vec<_> = ignored.into_iter().collect();
    ignored.sort_by(|a, b| b.1.cmp(&a.1));
    for (name, count) in ignored.iter().take(50) {
        let _ = writeln!(text, "{count:6}  {name}");
    }
    CssReport {
        headline,
        text,
        counts: c,
    }
}

/// Fold one sheet's `@media` blocks into the counts. A nested block
/// applies when it and every enclosing block do; its rules are counted
/// once, in the innermost block.
fn count_media(
    at_rules: &[css::AtRuleSeen],
    (width, height): (f32, f32),
    c: &mut CssCounts,
    kinds: &mut BTreeMap<&'static str, (usize, usize, usize)>,
) {
    // Per at-rule: (applies, should-not-apply), the enclosing chain folded in.
    let mut chain: Vec<(bool, bool)> = Vec::with_capacity(at_rules.len());
    let mut nested_rules = vec![0usize; at_rules.len()];
    for at_rule in at_rules {
        if at_rule.name == "media"
            && let Some(parent) = at_rule.parent
            && let Some(slot) = nested_rules.get_mut(parent)
        {
            *slot += at_rule.rules;
        }
    }
    for (index, at_rule) in at_rules.iter().enumerate() {
        let (outer_applies, outer_wrong) = at_rule
            .parent
            .and_then(|p| chain.get(p).copied())
            .unwrap_or((true, false));
        if at_rule.name != "media" {
            chain.push((outer_applies, outer_wrong));
            continue;
        }
        let verdict = css::media_verdict(&at_rule.prelude, width, height);
        let kind = media_kind(&at_rule.prelude);
        let applies = outer_applies && verdict.applies;
        let wrong = outer_wrong || kind.contains("(wrong");
        chain.push((applies, wrong));
        let rules = at_rule.rules.saturating_sub(nested_rules[index]);
        c.media += 1;
        c.media_rules += rules;
        c.media_invalid += verdict.invalid_queries;
        if applies {
            c.media_applied += rules;
        }
        if wrong {
            c.media_wrong += rules;
            if applies {
                c.media_wrong_applied += rules;
            }
        }
        let entry = kinds.entry(kind).or_default();
        entry.0 += 1;
        entry.1 += rules;
        if applies {
            entry.2 += rules;
        }
    }
}

fn origin_code(origin: Origin) -> &'static str {
    match origin {
        Origin::Ours => "ours",
        Origin::BrowserToo => "browser-too",
    }
}

/// What a `@media` condition is about, and whether it should match on a
/// 1280x800 light-mode screen (the probe's viewport). Crude, first match
/// wins; good enough to size the "every @media matches" gap.
fn media_kind(prelude: &str) -> &'static str {
    let p = prelude.to_ascii_lowercase().replace(' ', "");
    let max_width = p
        .split("max-width:")
        .nth(1)
        .and_then(|rest| rest.split("px").next())
        .and_then(|n| n.parse::<f32>().ok());
    let min_width = p
        .split("min-width:")
        .nth(1)
        .and_then(|rest| rest.split("px").next())
        .and_then(|n| n.parse::<f32>().ok());
    if p.contains("prefers-color-scheme:dark") {
        "prefers-color-scheme: dark (wrong at 1280x800 light)"
    } else if p.starts_with("print") || p.contains("onlyprint") {
        "print (wrong at 1280x800 light)"
    } else if p.contains("prefers-reduced-motion:reduce") {
        "prefers-reduced-motion: reduce (wrong)"
    } else if p.contains("forced-colors:active") || p.contains("-ms-high-contrast") {
        "forced-colors / high-contrast (wrong)"
    } else if max_width.is_some_and(|w| w < 1280.0) {
        "max-width < 1280 (wrong at 1280x800 light)"
    } else if min_width.is_some_and(|w| w > 1280.0) {
        "min-width > 1280 (wrong at 1280x800 light)"
    } else if min_width.is_some() || max_width.is_some() {
        "width query that does match 1280"
    } else if p.contains("hover") || p.contains("pointer") {
        "hover/pointer"
    } else if p.is_empty() || p == "screen" || p == "all" || p.starts_with("screen") {
        "screen / all"
    } else {
        "other"
    }
}

fn selector_cause(selector: &str) -> &'static str {
    let s = selector.to_ascii_lowercase();
    const CAUSES: &[(&str, &str)] = &[
        ("::-webkit-", "::-webkit-* pseudo-element"),
        ("::-moz-", "::-moz-* pseudo-element"),
        (":-webkit-", ":-webkit-* pseudo-class"),
        (":-moz-", ":-moz-* pseudo-class"),
        (":-ms-", ":-ms-* pseudo-class"),
        ("::placeholder", "::placeholder"),
        ("::selection", "::selection"),
        ("::before", "::before/::after"),
        ("::after", "::before/::after"),
        (":before", ":before/:after (legacy)"),
        (":after", ":before/:after (legacy)"),
        (":is(", ":is()"),
        (":where(", ":where()"),
        (":has(", ":has()"),
        (":focus-visible", ":focus-visible"),
        (":focus-within", ":focus-within"),
        (":not(", ":not()"),
        (":nth-", ":nth-*()"),
        (":first-child", ":first/last/only-child"),
        (":last-child", ":first/last/only-child"),
        (":only-child", ":first/last/only-child"),
        (":first-of-type", ":*-of-type"),
        (":last-of-type", ":*-of-type"),
        (":disabled", ":disabled/:enabled/:checked"),
        (":checked", ":disabled/:enabled/:checked"),
        (":empty", ":empty"),
        (":root", ":root"),
        ("::", "other ::pseudo-element"),
        ("&", "nesting (&)"),
    ];
    for (needle, cause) in CAUSES {
        if s.contains(needle) {
            return cause;
        }
    }
    if s.contains(':') {
        "other pseudo-class"
    } else {
        "other"
    }
}

// ---------------------------------------------------------------------------
// Layout anomalies
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Anomalies {
    boxes: usize,
    /// Boxes holding text that came out narrower than ~one glyph.
    slivers: Vec<String>,
    /// Large boxes (> 100×40) with no text and no element children painted
    /// inside, i.e. likely placeholders whose content went missing.
    offscreen: Vec<String>,
    text_boxes: usize,
    widest: f32,
    /// `<style>`/`<script>`/`<template>` elements that got a layout box
    /// (their source text would paint as page text).
    unrendered: Vec<String>,
}

impl Anomalies {
    fn headline(&self) -> String {
        format!(
            "{} boxes, {} with text, {} text slivers (<3px per char), {} boxes off the left/top edge, {} style/script boxes, widest box {:.0}px",
            self.boxes,
            self.text_boxes,
            self.slivers.len(),
            self.offscreen.len(),
            self.unrendered.len(),
            self.widest
        )
    }

    fn render(&self, page_width: f32) -> String {
        let mut text = format!("{}\npage width {page_width}\n", self.headline());
        let _ = writeln!(text, "\n## text slivers (first 60)");
        for line in self.slivers.iter().take(60) {
            let _ = writeln!(text, "{line}");
        }
        let _ = writeln!(
            text,
            "\n## style/script/template elements with a layout box"
        );
        for line in &self.unrendered {
            let _ = writeln!(text, "{line}");
        }
        let _ = writeln!(text, "\n## off-page boxes (first 40)");
        for line in self.offscreen.iter().take(40) {
            let _ = writeln!(text, "{line}");
        }
        text
    }
}

fn walk_layout(layout: &LayoutBox, document: &Document, _sheets: usize, out: &mut Anomalies) {
    walk_layout_in(layout, document, out, &mut Vec::new());
}

/// Each layout ancestor as `tag[display/position/float]`, for tracing
/// which layout path placed a box.
fn box_label(layout: &LayoutBox) -> String {
    let kind = match &layout.box_type {
        BoxType::BlockNode(_) => "B",
        BoxType::FlexNode(_) => "F",
        BoxType::GridNode(_) => "G",
        BoxType::TableNode(_) => "T",
        BoxType::AnonymousBlock => return "anon".into(),
    };
    let (BoxType::BlockNode(n)
    | BoxType::FlexNode(n)
    | BoxType::GridNode(n)
    | BoxType::TableNode(n)) = &layout.box_type
    else {
        unreachable!()
    };
    let tag = match &n.node_type {
        NodeType::Element(e) => e.tag_name.clone(),
        NodeType::Text(_) => "#text".into(),
    };
    let kw = |name: &str| match n.value(name) {
        Some(Value::Keyword(k)) => k.clone(),
        Some(_) => "?".into(),
        None => "-".into(),
    };
    format!(
        "{tag}:{kind}[{}/{}/{}]",
        kw("display"),
        kw("position"),
        kw("float")
    )
}

fn walk_layout_in(
    layout: &LayoutBox,
    document: &Document,
    out: &mut Anomalies,
    path: &mut Vec<String>,
) {
    let _ = &path;
    out.boxes += 1;
    let rect = layout.dimensions.content;
    out.widest = out.widest.max(rect.width);
    let styled = match &layout.box_type {
        BoxType::BlockNode(n)
        | BoxType::FlexNode(n)
        | BoxType::GridNode(n)
        | BoxType::TableNode(n) => Some(n),
        BoxType::AnonymousBlock => None,
    };
    if let Some(node) = styled {
        if let NodeType::Text(text) = &node.node_type {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                out.text_boxes += 1;
                if trimmed.chars().count() > 2
                    && rect.width / (trimmed.chars().count() as f32) < 3.0
                {
                    out.slivers.push(format!(
                        "{:7.1}x{:<7.1} at ({:.0},{:.0})  {:?}  in {}",
                        rect.width,
                        rect.height,
                        rect.x,
                        rect.y,
                        clip(trimmed, 24),
                        ancestry(document, node.node_id, 3)
                    ));
                }
            }
        } else if let NodeType::Element(element) = &node.node_type
            && matches!(element.tag_name.as_str(), "style" | "script" | "template")
        {
            out.unrendered.push(format!(
                "{} display={:?}  in {}\n    layout path: {}",
                element.tag_name,
                node.value("display"),
                ancestry(document, node.node_id, 3),
                path.iter()
                    .rev()
                    .take(6)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" < ")
            ));
        } else if rect.x + rect.width < -1.0 || rect.y + rect.height < -1.0 {
            out.offscreen.push(format!(
                "{:.0}x{:.0} at ({:.0},{:.0})  {}",
                rect.width,
                rect.height,
                rect.x,
                rect.y,
                ancestry(document, node.node_id, 2)
            ));
        }
    }
    path.push(box_label(layout));
    for child in &layout.children {
        walk_layout_in(child, document, out, path);
    }
    path.pop();
}

/// `div.cls#id < span.x < …` for the node and a few ancestors.
fn ancestry(document: &Document, mut id: NodeId, depth: usize) -> String {
    let mut parts = Vec::new();
    for _ in 0..=depth {
        let Some(node) = document.get(id) else {
            break;
        };
        if let NodeType::Element(element) = &node.node_type {
            let mut label = element.tag_name.clone();
            if let Some(id_attr) = element.attributes.get("id") {
                let _ = write!(label, "#{id_attr}");
            }
            if let Some(class) = element.attributes.get("class") {
                for c in class.split_whitespace().take(2) {
                    let _ = write!(label, ".{c}");
                }
            }
            parts.push(label);
        }
        match node.parent {
            Some(parent) => id = parent,
            None => break,
        }
    }
    parts.join(" < ")
}
