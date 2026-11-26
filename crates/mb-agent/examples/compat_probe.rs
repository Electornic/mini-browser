// Real-site compatibility probe (docs/compat-survey.md).
//
//   MB_NET_CACHE=compat-cache/net cargo run -p mb-agent --example compat_probe -- URL OUT_DIR
//
// Loads URL the way an agent would (AgentBrowser::navigate, system fonts),
// then writes to OUT_DIR:
//   observation.txt  what the agent sees (viewport observation + whole page)
//   css.txt          what the stylesheet parser dropped, grouped by cause,
//                    and parsed properties nothing downstream reads
//   layout.txt       layout anomalies (text squeezed into slivers, big empty
//                    boxes, boxes pushed off-page)
//   summary.txt      the headline numbers
//
// Always run it with MB_NET_CACHE (see crates/mb-runtime/src/net/cache.rs)
// so the site is fetched once, and keep OUT_DIR out of the repo: it holds
// third-party text.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use mb_agent::{AgentBrowser, AgentConfig, ClockMode};
use mb_runtime::{
    css::{self, Value},
    dom::{Document, NodeId, NodeType},
    layout::{BoxType, LayoutBox},
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
    let url = args.next().expect("usage: compat_probe URL OUT_DIR");
    let out = PathBuf::from(args.next().expect("usage: compat_probe URL OUT_DIR"));
    fs::create_dir_all(&out).expect("create OUT_DIR");

    let mut browser = AgentBrowser::new(AgentConfig {
        system_fonts: true,
        clock: ClockMode::Virtual,
        settle_timeout: Duration::from_secs(20),
        ..AgentConfig::default()
    });
    let viewport = match browser.navigate(&url) {
        Ok(observation) => observation,
        Err(error) => {
            eprintln!("navigate failed: {error:?}");
            std::process::exit(1);
        }
    };
    let whole = browser.full_snapshot();
    let mut observation = String::new();
    let _ = writeln!(
        observation,
        "=== viewport observation ===\n{}",
        viewport.to_text()
    );
    let _ = writeln!(observation, "\n=== whole page ===\n{}", whole.to_text());
    write(&out, "observation.txt", &observation);

    let state = browser.state();
    let document = state.parsed_document.borrow();
    let inline_css = inline_style_text(&document);
    let css_report = css_drops(&state.stylesheet, &inline_css);
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

    let mut summary = String::new();
    let _ = writeln!(summary, "url: {url}");
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
        "observation: {} elements (whole page), {} in viewport, {} text blocks, console {}",
        whole.total_elements,
        viewport.elements.len(),
        whole.text.len(),
        viewport.console.len()
    );
    for message in &viewport.console {
        let _ = writeln!(
            summary,
            "  console.{}: {}",
            message.level,
            clip(&message.text, 200)
        );
    }
    let _ = writeln!(summary, "layout: {}", anomalies.headline());
    let _ = writeln!(summary, "document height: {}", whole.document_height);
    write(&out, "summary.txt", &summary);
    print!("{summary}");
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
            for child in &node.children {
                if let Some(NodeType::Text(text)) = document.get(*child).map(|n| &n.node_type) {
                    css.push_str(text);
                    css.push('\n');
                }
            }
        }
        stack.extend(node.children.iter().rev().copied());
    }
    css
}

// ---------------------------------------------------------------------------
// CSS drops
// ---------------------------------------------------------------------------

struct CssReport {
    headline: String,
    text: String,
}

fn css_drops(external: &str, inline: &str) -> CssReport {
    let mut text = String::new();
    let mut totals = (0usize, 0usize, 0usize, 0usize);
    let mut declared = 0usize;
    let mut ignored: BTreeMap<String, usize> = BTreeMap::new();
    let mut dropped_props: BTreeMap<String, (usize, usize, String)> = BTreeMap::new();
    let mut rule_causes: BTreeMap<&'static str, (usize, String)> = BTreeMap::new();
    let mut at_rules: BTreeMap<String, (usize, bool)> = BTreeMap::new();
    let mut media_queries = 0usize;
    let mut media_kinds: BTreeMap<&'static str, usize> = BTreeMap::new();

    for source in [external, inline] {
        let (sheet, report) = css::parse_with_report(source);
        totals.0 += sheet.rules.len();
        totals.1 += report.dropped_rules.len();
        totals.2 += report.dropped_declarations.len();
        for rule in &sheet.rules {
            for decl in &rule.declarations {
                declared += 1;
                if !decl.name.starts_with("--") && !CONSUMED.contains(&decl.name.as_str()) {
                    *ignored.entry(decl.name.clone()).or_default() += 1;
                }
                if matches!(&decl.value, Value::Keyword(k) if k.contains('(') || k.ends_with("vw") || k.ends_with("vh"))
                {
                    totals.3 += 1;
                }
            }
        }
        for decl in &report.dropped_declarations {
            let name = decl
                .split(':')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            let entry = dropped_props.entry(name).or_insert((0, 0, decl.clone()));
            entry.0 += 1;
            if decl.contains("!important") {
                entry.1 += 1;
            }
        }
        for selector in &report.dropped_rules {
            let entry = rule_causes
                .entry(selector_cause(selector))
                .or_insert((0, selector.clone()));
            entry.0 += 1;
        }
        for at_rule in &report.at_rules {
            if at_rule.name == "media" {
                media_queries += 1;
                *media_kinds.entry(media_kind(&at_rule.prelude)).or_default() += 1;
            }
            let entry = at_rules
                .entry(at_rule.name.clone())
                .or_insert((0, at_rule.kept));
            entry.0 += 1;
        }
    }

    let important = dropped_props.values().map(|v| v.1).sum::<usize>();
    let headline = format!(
        "css: {} rules kept, {} rules dropped (selector), {} declarations dropped ({} with !important), {} declarations kept, {} @media (all treated as matching)",
        totals.0, totals.1, totals.2, important, declared, media_queries
    );
    let _ = writeln!(text, "{headline}\n");

    let _ = writeln!(
        text,
        "## dropped declarations by property (count, !important, sample)"
    );
    let mut props: Vec<_> = dropped_props.into_iter().collect();
    props.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    for (name, (count, imp, sample)) in props.iter().take(60) {
        let _ = writeln!(text, "{count:6} {imp:6}  {name:28} {}", clip(sample, 90));
    }

    let _ = writeln!(text, "\n## dropped rules by selector cause (count, sample)");
    let mut causes: Vec<_> = rule_causes.into_iter().collect();
    causes.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    for (cause, (count, sample)) in causes {
        let _ = writeln!(text, "{count:6}  {cause:28} {}", clip(&sample, 90));
    }

    let _ = writeln!(text, "\n## at-rules (count, body kept?)");
    for (name, (count, kept)) in &at_rules {
        let _ = writeln!(
            text,
            "{count:6}  @{name} {}",
            if *kept { "kept" } else { "SKIPPED" }
        );
    }

    let _ = writeln!(
        text,
        "\n## @media conditions (all applied today; 'wrong at 1280x800 light' = should NOT apply)"
    );
    for (kind, count) in &media_kinds {
        let _ = writeln!(text, "{count:6}  {kind}");
    }

    let _ = writeln!(text, "\n## parsed but never read downstream (count)");
    let mut ignored: Vec<_> = ignored.into_iter().collect();
    ignored.sort_by(|a, b| b.1.cmp(&a.1));
    for (name, count) in ignored.iter().take(50) {
        let _ = writeln!(text, "{count:6}  {name}");
    }
    let _ = writeln!(
        text,
        "\nkeyword-shaped values that look like unsupported functions/units (calc, vw…): {}",
        totals.3
    );
    CssReport { headline, text }
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
