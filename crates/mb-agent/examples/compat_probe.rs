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
    layout::{self, BoxType, ClipTable, LayoutBox, Rect},
    net,
    render::{self, Affine},
    style,
};

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
    // One more style pass, the same one the last frame ran (default
    // interaction state), this time with the computed-value-time report:
    // IACVT and keyword fallbacks only exist after substitution.
    let style_report = document.roots().last().map(|&root| {
        style::style_tree_with_report(
            &document,
            root,
            &state.parsed_stylesheets,
            style::InteractionState::default(),
            &css::media::MediaContext::new(media_ctx.0, media_ctx.1),
        )
        .1
    });
    // Step 4: the browser's own style pass (no report), min of five — the
    // F4 measurement, now a column.
    let style_ms = document.roots().last().map_or(0.0, |&root| {
        min_ms(5, || {
            style::style_tree_with_state(
                &document,
                root,
                &state.parsed_stylesheets,
                style::InteractionState::default(),
                &css::media::MediaContext::new(media_ctx.0, media_ctx.1),
            )
        })
    });
    let mut css_report = css_drops(
        &state.stylesheet,
        &inline_css,
        media_ctx,
        &style_report.unwrap_or_default(),
    );
    css_report.counts.media_attr_sheets = media_attr_sheets(&document);
    css_report.counts.style_ms = style_ms;
    write(&out, "css.txt", &css_report.text);

    let view = state.current_view().expect("a laid-out view");
    let mut anomalies = Anomalies {
        page_width: browser.config().viewport_width as f32,
        ..Anomalies::default()
    };
    // The same per-box clip table paint, hit testing and the agent's click
    // target read (step 3) — computed here from the frame's layout.
    let clips = layout::compute_clips(&view.layout_root);
    anomalies.clip_boxes = clips.len();
    walk_layout(&view.layout_root, &document, &clips, &mut anomalies);
    anomalies.elements_clipped_out = whole
        .elements
        .iter()
        .filter(|e| anomalies.element_visible.get(&e.id) == Some(&false))
        .map(|e| format!("{}\t{}", e.role, one_line(&e.name)))
        .collect();
    // Step 3: the cost of the scissor — rasterise the first viewport of the
    // frame's list as is and with every `ClipGroup` flattened (what an
    // unclipped engine would paint), median of three each.
    let (width, height) = (
        browser.config().viewport_width,
        browser.config().viewport_height,
    );
    let mut groups = 0;
    render::walk_commands(&view.commands, &mut |c| {
        groups += usize::from(matches!(c, render::DisplayCommand::ClipGroup(..)));
    });
    anomalies.clip_groups = groups;
    let flat = flatten_clips(&view.commands);
    anomalies.raster_ms = median_ms(|| render::rasterize(&view.commands, width, height));
    anomalies.raster_unclipped_ms = median_ms(|| render::rasterize(&flat, width, height));
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
        widest: anomalies.widest,
        clip: anomalies.clip_counts(),
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
    widest: f32,
    clip: ClipCounts,
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
    col("iacvt", &|c| c.css.iacvt.to_string());
    col("var_keyword_fallback", &|c| {
        c.css.var_keyword_fallback.to_string()
    });
    col("decl_drop_total", &|c| c.css.drop_total().to_string());
    // Step 3 (overflow clipping): see `ClipCounts`.
    col("clip_boxes", &|c| c.clip.clip_boxes.to_string());
    col("slivers_in_clip", &|c| c.clip.slivers_in_clip.to_string());
    col("slivers_in_sr", &|c| c.clip.slivers_in_sr.to_string());
    col("slivers_painted", &|c| c.clip.slivers_painted.to_string());
    col("slivers_sr_painted", &|c| {
        c.clip.slivers_sr_painted.to_string()
    });
    col("text_hidden", &|c| c.clip.text_hidden.to_string());
    col("text_hidden_sr", &|c| c.clip.text_hidden_sr.to_string());
    col("text_hidden_offpage", &|c| {
        c.clip.text_hidden_offpage.to_string()
    });
    col("text_hidden_other", &|c| {
        c.clip.text_hidden_other.to_string()
    });
    col("text_cut", &|c| c.clip.text_cut.to_string());
    col("clip_groups", &|c| c.clip.clip_groups.to_string());
    col("elements_clipped_out", &|c| {
        c.clip.elements_clipped_out.to_string()
    });
    col("raster_ms", &|c| format!("{:.1}", c.clip.raster_ms));
    col("raster_unclipped_ms", &|c| {
        format!("{:.1}", c.clip.raster_unclipped_ms)
    });
    // Step 4 (selectors): rules dropped over a pseudo, ours only.
    col("rule_drop_step4", &|c| c.css.rule_step4.to_string());
    col("rule_drop_pseudo_element", &|c| {
        c.css.rule_pseudo_element.to_string()
    });
    col("rule_drop_no_pseudo", &|c| c.css.rule_no_pseudo.to_string());
    col("rule_blockers", &|c| c.css.rule_blockers.clone());
    col("style_ms", &|c| format!("{:.1}", c.css.style_ms));
    // Step 5 (calc / viewport units / grid track functions): drops in
    // `decl_drop_total` whose value has a step-5 feature
    // (`css::value_features`), by kind, and `feature:drops` over all kinds.
    col("drop_step5", &|c| {
        c.css.step5.iter().sum::<usize>().to_string()
    });
    col("drop_step5_ours", &|c| c.css.step5[0].to_string());
    col("drop_step5_iacvt", &|c| c.css.step5[1].to_string());
    col("drop_step5_keyword", &|c| {
        (c.css.step5[2] + c.css.step5[3]).to_string()
    });
    col("drop_step5_causes", &|c| c.css.step5_causes.clone());
    col("widest_box", &|c| format!("{:.0}", c.widest));
    col("at_skipped_ours", &|c| c.css.at_ours.to_string());
    col("at_skipped_browser", &|c| c.css.at_browser.to_string());
    col("at_skipped_bytes", &|c| c.css.at_skipped_bytes.to_string());
    col("at_skipped_pct", &|c| {
        format!(
            "{:.1}",
            c.css.at_skipped_bytes as f64 * 100.0 / c.css_bytes.max(1) as f64
        )
    });
    col("at_skipped_by_name", &|c| c.css.at_skipped_by_name.clone());
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
    col("layer_blocks", &|c| c.css.layer_blocks.to_string());
    col("layer_statements", &|c| c.css.layer_statements.to_string());
    col("layer_rules", &|c| c.css.layer_rules.to_string());
    col("supports", &|c| c.css.supports.to_string());
    col("supports_true", &|c| c.css.supports_true.to_string());
    col("supports_false", &|c| c.css.supports_false.to_string());
    col("supports_rules", &|c| c.css.supports_rules.to_string());
    col("supports_applied", &|c| c.css.supports_applied.to_string());
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
    /// `name:bytes` of skipped at-rule bodies, largest first.
    at_skipped_by_name: String,
    /// Kept declarations of a property with a modelled value kind whose
    /// value is a `Keyword` that kind doesn't accept (`width: 100vw`).
    keyword_fallback: usize,
    /// Kept declarations nothing downstream reads (`properties::RENDERED`).
    parse_only: usize,
    /// Kept declarations invalid at computed-value time on at least one
    /// node (`style::StyleReport::iacvt`) — a `var()` that didn't resolve.
    iacvt: usize,
    /// Kept declarations whose `var()` substitution left a keyword the
    /// property rejects (`StyleReport::var_keyword_fallback`).
    var_keyword_fallback: usize,
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
    /// `@layer` with a block (named or anonymous; nested ones included,
    /// once the engine reads layer bodies) / statements (`@layer a, b;`).
    layer_blocks: usize,
    layer_statements: usize,
    /// Rules the engine kept from inside `@layer` blocks.
    layer_rules: usize,
    /// `@supports` blocks / of those, whose condition the engine holds
    /// true or false (on its own, ignoring enclosing blocks).
    supports: usize,
    supports_true: usize,
    supports_false: usize,
    /// Rules the engine kept from inside `@supports` blocks / of those,
    /// the ones it applies at the probe's viewport (every enclosing
    /// condition included).
    supports_rules: usize,
    supports_applied: usize,
    /// `code:count` of every declaration drop reason.
    decl_reasons: String,
    /// Dropped rules (ours) blamed on a pseudo step 4 makes parse
    /// ([`STEP4_BLOCKERS`]) / on any pseudo-element / on no pseudo at all
    /// (nesting, namespaces, syntax).
    rule_step4: usize,
    rule_pseudo_element: usize,
    rule_no_pseudo: usize,
    /// `label:rules` over dropped rules (ours), most first — a rule counts
    /// once under every pseudo that blocks it (`css::selector_blockers`).
    rule_blockers: String,
    /// The browser's style pass at the probe viewport, min of five (ms).
    style_ms: f64,
    /// Drops in [`CssCounts::drop_total`] with a step-5 feature
    /// (`css::STEP5_FEATURES`): parser drops (ours) / IACVT / keyword
    /// fallback / var keyword fallback.
    step5: [usize; 4],
    /// `feature:drops` over every drop in the total, most first — a drop
    /// counts once under each feature in its value (non-step-5 `math()` /
    /// `env()` included).
    step5_causes: String,
}

/// The pseudos step 4 (docs/compat-round3.md 4단계) makes parse — once it
/// is done, no rule (ours) is blamed on one of these.
const STEP4_BLOCKERS: &[&str] = &[
    ":is()",
    ":where()",
    ":focus-visible",
    ":focus-within",
    ":checked",
    ":disabled",
    ":enabled",
    "::before",
    "::after",
    "::placeholder",
    "::marker",
    "::selection",
    "::first-line",
    "::first-letter",
    "::backdrop",
    "::file-selector-button",
    "::-webkit-*",
];

impl CssCounts {
    /// The step-1 target metric (docs/compat-round3.md "측정 프로토콜"):
    /// parser drops (ours) + invalid at computed-value time + both keyword
    /// fallbacks — so a failure moving from parse time to substitution
    /// time doesn't vanish from the count.
    fn drop_total(&self) -> usize {
        self.decl_ours + self.iacvt + self.keyword_fallback + self.var_keyword_fallback
    }
}

fn css_drops(
    external: &str,
    inline: &str,
    (width, height): (f32, f32),
    computed: &style::StyleReport,
) -> CssReport {
    let mut text = String::new();
    let mut c = CssCounts::default();
    // property → (count, sample) of the computed-value-time failures
    let mut iacvt_props: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let mut var_fallback_props: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let mut ignored: BTreeMap<String, usize> = BTreeMap::new();
    let mut fallbacks: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let mut dropped_props: BTreeMap<(Origin, DropReason, String), (usize, String)> =
        BTreeMap::new();
    let mut decl_reasons: BTreeMap<DropReason, usize> = BTreeMap::new();
    let mut rule_causes: BTreeMap<(Origin, &'static str), (usize, String)> = BTreeMap::new();
    // (origin, blocking pseudo) → (rules, sample)
    let mut blocker_rules: BTreeMap<(Origin, String), (usize, String)> = BTreeMap::new();
    let mut at_rules: BTreeMap<String, (usize, bool, usize)> = BTreeMap::new();
    // kind → (blocks, rules, rules applied)
    let mut media_kinds: BTreeMap<&'static str, (usize, usize, usize)> = BTreeMap::new();
    // prelude → (blocks, verdict, rules, rules applied)
    let mut supports_conditions: BTreeMap<String, (usize, bool, usize, usize)> = BTreeMap::new();
    // layer name → (blocks, statements, rules)
    let mut layer_names: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    // (feature, kind, property) → (drops, sample), and feature → drops
    let mut features: BTreeMap<(&'static str, &'static str, String), (usize, String)> =
        BTreeMap::new();
    let mut feature_drops: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut record_features =
        |c: &mut CssCounts, kind: usize, property: &str, found: &[&'static str], sample: &str| {
            const KINDS: [&str; 4] = ["ours", "iacvt", "keyword", "var-keyword"];
            if found.iter().any(|f| css::STEP5_FEATURES.contains(f)) {
                c.step5[kind] += 1;
            }
            let labels: &[&'static str] = if found.is_empty() { &["(none)"] } else { found };
            for &label in labels {
                if label != "(none)" {
                    *feature_drops.entry(label).or_default() += 1;
                }
                let entry = features
                    .entry((label, KINDS[kind], property.to_string()))
                    .or_insert((0, sample.to_string()));
                entry.0 += 1;
            }
        };

    for source in [external, inline] {
        let (sheet, report) = css::parse_with_report(source);
        c.rule_kept += sheet.rules.len();
        for rule in &sheet.rules {
            // A key counts once per rule (a shorthand expanded into
            // longhands that share one declared value is one declaration).
            let mut seen_keys: Vec<String> = Vec::new();
            for decl in &rule.declarations {
                let key = style::declaration_key(&decl.name, &decl.value);
                let first_in_rule = !seen_keys.contains(&key);
                if first_in_rule {
                    let failed = if computed.iacvt.contains(&key) {
                        c.iacvt += 1;
                        Some((1, &mut iacvt_props))
                    } else if computed.var_keyword_fallback.contains(&key) {
                        c.var_keyword_fallback += 1;
                        Some((3, &mut var_fallback_props))
                    } else {
                        None
                    };
                    let failed = failed.map(|(kind, props)| {
                        // What the re-parse saw; none = `var()` itself failed.
                        let text = computed.reparsed_text.get(&key);
                        let found = text
                            .map_or_else(Vec::new, |text| css::value_features(&decl.name, text));
                        let sample = text.map_or_else(|| key.clone(), |t| format!("{key} → {t}"));
                        record_features(&mut c, kind, &decl.name, &found, &sample);
                        props
                    });
                    if let Some(props) = failed {
                        let entry = props.entry(decl.name.clone()).or_insert((0, key.clone()));
                        entry.0 += 1;
                    }
                    seen_keys.push(key);
                }
                c.decl_kept += 1;
                if !decl.name.starts_with("--") && !properties::is_rendered(&decl.name) {
                    c.parse_only += 1;
                    *ignored.entry(decl.name.clone()).or_default() += 1;
                }
                if let (Some(kind), Value::Keyword(keyword)) =
                    (properties::lookup(&decl.name), &decl.value)
                    && kind.is_typed()
                    && !kind.accepts_keyword(keyword)
                {
                    c.keyword_fallback += 1;
                    let found = css::value_features(&decl.name, keyword);
                    record_features(
                        &mut c,
                        2,
                        &decl.name,
                        &found,
                        &format!("{}: {keyword}", decl.name),
                    );
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
                record_features(&mut c, 0, &decl.property, &decl.features, &decl.text);
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
            let ours = rule.reason.origin() == Origin::Ours;
            let fallback = [if rule.over_nesting_cap {
                "(combinator nesting cap)".to_string()
            } else if rule.selector.contains('&') {
                "(nesting &)".to_string()
            } else {
                "(no pseudo)".to_string()
            }];
            let labels = if rule.blockers.is_empty() {
                &fallback[..]
            } else {
                &rule.blockers[..]
            };
            for label in labels {
                let entry = blocker_rules
                    .entry((rule.reason.origin(), label.clone()))
                    .or_insert((0, rule.selector.clone()));
                entry.0 += 1;
            }
            if ours {
                c.rule_step4 += usize::from(
                    rule.blockers
                        .iter()
                        .any(|b| STEP4_BLOCKERS.contains(&b.as_str())),
                );
                c.rule_pseudo_element +=
                    usize::from(rule.blockers.iter().any(|b| b.starts_with("::")));
                c.rule_no_pseudo += usize::from(rule.blockers.is_empty());
            }
        }
        count_blocks(
            &report.at_rules,
            (width, height),
            &mut c,
            BlockTables {
                media_kinds: &mut media_kinds,
                supports_conditions: &mut supports_conditions,
                layer_names: &mut layer_names,
            },
        );
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
    let mut by_drops: Vec<_> = feature_drops.into_iter().collect();
    by_drops.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    c.step5_causes = by_drops
        .iter()
        .map(|(label, n)| format!("{label}:{n}"))
        .collect::<Vec<_>>()
        .join(",");
    c.decl_reasons = decl_reasons
        .iter()
        .map(|(reason, count)| format!("{}:{count}", reason.code()))
        .collect::<Vec<_>>()
        .join(",");
    let mut skipped_by_name: Vec<_> = at_rules
        .iter()
        .filter(|(_, (_, _, bytes))| *bytes > 0)
        .map(|(name, (_, _, bytes))| (*bytes, name.as_str()))
        .collect();
    skipped_by_name.sort_by(|a, b| b.cmp(a));
    c.at_skipped_by_name = skipped_by_name
        .iter()
        .map(|(bytes, name)| format!("{name}:{bytes}"))
        .collect::<Vec<_>>()
        .join(",");

    let headline = format!(
        "css: {} rules kept, rules dropped {} ours / {} browser-too, declarations dropped {} ours / {} browser-too ({}), {} declarations kept ({} keyword fallbacks, {} parse-only; computed time: {} invalid (IACVT), {} var keyword fallbacks; drop total {}), {} nested rules, {} @import, {} @media ({} rules, {} applied; {} should not apply, {} of them applied; {} invalid queries), {} @layer blocks + {} statements ({} rules), {} @supports ({} true / {} false; {} rules, {} applied), at-rules skipped {} ours / {} browser-too ({} bytes)",
        c.rule_kept,
        c.rule_ours,
        c.rule_browser,
        c.decl_ours,
        c.decl_browser,
        c.decl_reasons,
        c.decl_kept,
        c.keyword_fallback,
        c.parse_only,
        c.iacvt,
        c.var_keyword_fallback,
        c.drop_total(),
        c.nesting,
        c.imports,
        c.media,
        c.media_rules,
        c.media_applied,
        c.media_wrong,
        c.media_wrong_applied,
        c.media_invalid,
        c.layer_blocks,
        c.layer_statements,
        c.layer_rules,
        c.supports,
        c.supports_true,
        c.supports_false,
        c.supports_rules,
        c.supports_applied,
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

    let _ = writeln!(
        text,
        "\n## dropped rules by blocking pseudo (rules, sample; a rule counts under each pseudo that blocks it)"
    );
    let mut blockers: Vec<_> = blocker_rules.into_iter().collect();
    blockers.sort_by(|a, b| {
        a.0.0
            .cmp(&b.0.0)
            .then(b.1.0.cmp(&a.1.0))
            .then(a.0.1.cmp(&b.0.1))
    });
    let mut ours_blockers = Vec::new();
    for ((origin, label), (count, sample)) in &blockers {
        if *origin == Origin::Ours {
            ours_blockers.push(format!("{label}:{count}"));
        }
        let step4 = if STEP4_BLOCKERS.contains(&label.as_str()) {
            "step4"
        } else {
            "-"
        };
        let _ = writeln!(
            text,
            "{count:6}  {:11} {step4:5} {label:28} {}",
            origin_code(*origin),
            clip(sample, 80)
        );
    }
    c.rule_blockers = ours_blockers.join(",");

    let _ = writeln!(
        text,
        "\n## step 5: dropped declarations (the drop total) by value feature / kind / property (drops, sample; a drop counts under each feature; (none) = no feature — for iacvt, var() itself failed)"
    );
    let mut rows: Vec<_> = features.into_iter().collect();
    rows.sort_by(|a, b| {
        let step5 = |f: &str| !css::STEP5_FEATURES.contains(&f);
        step5(a.0.0)
            .cmp(&step5(b.0.0))
            .then(b.1.0.cmp(&a.1.0))
            .then(a.0.cmp(&b.0))
    });
    for ((label, kind, property), (count, sample)) in rows.iter().take(120) {
        let _ = writeln!(
            text,
            "{count:6}  {label:14} {kind:11} {property:28} {}",
            clip(sample, 100)
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
        "\n## @supports conditions (blocks, engine verdict, rules, rules applied at {width}x{height}; most blocks first)"
    );
    let mut conditions: Vec<_> = supports_conditions.into_iter().collect();
    conditions.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(&b.0)));
    for (prelude, (blocks, verdict, rules, applied)) in conditions.iter().take(60) {
        let _ = writeln!(
            text,
            "{blocks:6} {:5} {rules:6} {applied:6}  {}",
            if *verdict { "true" } else { "false" },
            clip(prelude, 100)
        );
    }

    let _ = writeln!(
        text,
        "\n## @layer names (blocks, statements naming it, rules; '' = anonymous)"
    );
    for (name, (blocks, statements, rules)) in &layer_names {
        let _ = writeln!(text, "{blocks:6} {statements:6} {rules:6}  {name}");
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

    for (title, props) in [
        (
            "invalid at computed-value time (IACVT): declarations whose var() didn't resolve on some node (count, sample)",
            iacvt_props,
        ),
        (
            "var keyword fallbacks: substitution left a keyword the property rejects (count, sample)",
            var_fallback_props,
        ),
    ] {
        let _ = writeln!(text, "\n## {title}");
        let mut props: Vec<_> = props.into_iter().collect();
        props.sort_by(|a, b| b.1.0.cmp(&a.1.0));
        for (name, (count, sample)) in props.iter().take(40) {
            let _ = writeln!(text, "{count:6}  {name:28} {}", clip(sample, 80));
        }
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

/// The per-condition / per-name tables [`count_blocks`] fills.
struct BlockTables<'a> {
    media_kinds: &'a mut BTreeMap<&'static str, (usize, usize, usize)>,
    supports_conditions: &'a mut BTreeMap<String, (usize, bool, usize, usize)>,
    layer_names: &'a mut BTreeMap<String, (usize, usize, usize)>,
}

/// What an at-rule's enclosing chain says about the rules directly in it.
#[derive(Clone, Copy)]
struct Chain {
    /// The engine applies them (every enclosing condition holds).
    applies: bool,
    /// Some enclosing `@media` should not match at 1280x800 light.
    wrong: bool,
    /// The innermost enclosing `@media` (self included): its kind.
    media_kind: Option<&'static str>,
    /// The innermost enclosing `@layer` / `@supports` (self included):
    /// its index in the sheet's at-rules.
    layer: Option<usize>,
    supports: Option<usize>,
}

/// Fold one sheet's at-rules into the counts. A nested block applies when
/// it and every enclosing block do; a rule is counted once, in the
/// innermost block, and attributed to every kind of block around it (a
/// rule in `@layer x { @media … { } }` is both a layer rule and a media
/// rule).
fn count_blocks(
    at_rules: &[css::AtRuleSeen],
    (width, height): (f32, f32),
    c: &mut CssCounts,
    tables: BlockTables<'_>,
) {
    let mut chain: Vec<Chain> = Vec::with_capacity(at_rules.len());
    // Rules of the kept blocks directly inside each at-rule.
    let mut nested_rules = vec![0usize; at_rules.len()];
    for at_rule in at_rules {
        // A false `@supports` body is parsed (its rules counted in it) but
        // hands none to the enclosing block.
        let handed_up = if at_rule.name == "supports" && !css::supports_verdict(&at_rule.prelude) {
            0
        } else {
            at_rule.rules
        };
        if let Some(parent) = at_rule.parent
            && let Some(slot) = nested_rules.get_mut(parent)
        {
            *slot += handed_up;
        }
    }
    let top = Chain {
        applies: true,
        wrong: false,
        media_kind: None,
        layer: None,
        supports: None,
    };
    for (index, at_rule) in at_rules.iter().enumerate() {
        let outer = at_rule
            .parent
            .and_then(|p| chain.get(p).copied())
            .unwrap_or(top);
        let mut here = outer;
        match at_rule.name.as_str() {
            "media" => {
                let verdict = css::media_verdict(&at_rule.prelude, width, height);
                let kind = media_kind(&at_rule.prelude);
                here.applies &= verdict.applies;
                here.wrong |= kind.contains("(wrong");
                here.media_kind = Some(kind);
                c.media += 1;
                c.media_invalid += verdict.invalid_queries;
                tables.media_kinds.entry(kind).or_default().0 += 1;
            }
            "supports" if at_rule.has_block => {
                let verdict = css::supports_verdict(&at_rule.prelude);
                here.applies &= verdict;
                here.supports = Some(index);
                c.supports += 1;
                if verdict {
                    c.supports_true += 1;
                } else {
                    c.supports_false += 1;
                }
                let entry = tables
                    .supports_conditions
                    .entry(at_rule.prelude.clone())
                    .or_insert((0, verdict, 0, 0));
                entry.0 += 1;
            }
            "layer" => {
                if at_rule.has_block {
                    here.layer = Some(index);
                    c.layer_blocks += 1;
                    tables
                        .layer_names
                        .entry(at_rule.prelude.trim().to_string())
                        .or_default()
                        .0 += 1;
                } else {
                    c.layer_statements += 1;
                    for name in at_rule.prelude.split(',') {
                        tables
                            .layer_names
                            .entry(name.trim().to_string())
                            .or_default()
                            .1 += 1;
                    }
                }
            }
            _ => {}
        }
        chain.push(here);
        // Rules directly in this block (0 for a skipped one).
        let rules = at_rule.rules.saturating_sub(nested_rules[index]);
        let applied = if here.applies { rules } else { 0 };
        if let Some(kind) = here.media_kind {
            c.media_rules += rules;
            c.media_applied += applied;
            if here.wrong {
                c.media_wrong += rules;
                c.media_wrong_applied += applied;
            }
            let entry = tables.media_kinds.entry(kind).or_default();
            entry.1 += rules;
            entry.2 += applied;
        }
        if let Some(supports) = here.supports {
            c.supports_rules += rules;
            c.supports_applied += applied;
            if let Some(entry) = at_rules
                .get(supports)
                .and_then(|s| tables.supports_conditions.get_mut(&s.prelude))
            {
                entry.2 += rules;
                entry.3 += applied;
            }
        }
        if let Some(layer) = here.layer {
            c.layer_rules += rules;
            if let Some(entry) = at_rules
                .get(layer)
                .and_then(|l| tables.layer_names.get_mut(l.prelude.trim()))
            {
                entry.2 += rules;
            }
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
    /// The widest box: its element ancestry, declared `width`, and layout
    /// path (step 5: GitHub's widest box should reach viewport scale).
    widest_label: String,
    /// `<style>`/`<script>`/`<template>` elements that got a layout box
    /// (their source text would paint as page text).
    unrendered: Vec<String>,
    // --- step 3 (overflow clipping) ---
    /// Nodes the engine's `ClipTable` clips.
    clip_boxes: usize,
    /// Slivers under an ancestor that clips by style (`ClipStyle`, any kind).
    slivers_in_clip: usize,
    /// Slivers under an sr-only-like ancestor (`ClipStyle::sr`).
    slivers_in_sr: usize,
    /// Slivers the engine still paints (non-empty after the `ClipTable`).
    slivers_painted: usize,
    /// … of which under an sr-only-like ancestor — step 3's target is 0.
    slivers_sr_painted: usize,
    /// Text boxes with area that the `ClipTable` hides completely.
    text_hidden_sr: usize,
    /// The same outside any sr-only-like ancestor, laid out wholly beside
    /// the page (x past its right edge or before 0 — off-screen carousel
    /// slides, panels): never on screen, clipped or not.
    text_hidden_offpage: usize,
    /// … and the rest: on-page text a real-sized clip hides (collapsed
    /// list, line clamp — or a wrong clip rule / a box our layout put
    /// outside its container). Step 3's worst failure: each must be
    /// explained (listed in layout.txt, grouped by clip ancestor).
    text_hidden_other: Vec<String>,
    text_hidden_other_by_clip: BTreeMap<String, usize>,
    /// Page width, for the off-page test.
    page_width: f32,
    /// Per element node: does any of its boxes show through its clip?
    element_visible: BTreeMap<NodeId, bool>,
    /// Observed elements whose every box is clipped away — the agent's
    /// click refuses them ("clipped out of view", step 3b).
    elements_clipped_out: Vec<String>,
    /// Text boxes the `ClipTable` cuts but doesn't hide.
    text_cut: usize,
    /// … listed (first 100) with the visible part, for reviewing a change.
    text_cut_lines: Vec<String>,
    /// `ClipGroup` commands in the frame's display list.
    clip_groups: usize,
    /// First-viewport raster time, and the same list unclipped.
    raster_ms: f64,
    raster_unclipped_ms: f64,
}

/// The step 3 columns of `summary.tsv`.
#[derive(Default)]
struct ClipCounts {
    clip_boxes: usize,
    slivers_in_clip: usize,
    slivers_in_sr: usize,
    slivers_painted: usize,
    slivers_sr_painted: usize,
    text_hidden: usize,
    text_hidden_sr: usize,
    text_hidden_offpage: usize,
    text_hidden_other: usize,
    text_cut: usize,
    clip_groups: usize,
    raster_ms: f64,
    raster_unclipped_ms: f64,
    elements_clipped_out: usize,
}

impl Anomalies {
    fn clip_counts(&self) -> ClipCounts {
        ClipCounts {
            clip_boxes: self.clip_boxes,
            slivers_in_clip: self.slivers_in_clip,
            slivers_in_sr: self.slivers_in_sr,
            slivers_painted: self.slivers_painted,
            slivers_sr_painted: self.slivers_sr_painted,
            text_hidden: self.text_hidden_sr
                + self.text_hidden_offpage
                + self.text_hidden_other.len(),
            text_hidden_sr: self.text_hidden_sr,
            text_hidden_offpage: self.text_hidden_offpage,
            text_hidden_other: self.text_hidden_other.len(),
            text_cut: self.text_cut,
            clip_groups: self.clip_groups,
            raster_ms: self.raster_ms,
            raster_unclipped_ms: self.raster_unclipped_ms,
            elements_clipped_out: self.elements_clipped_out.len(),
        }
    }

    fn clip_headline(&self) -> String {
        format!(
            "clipping: {} clipped boxes; slivers {} in a clip by style ({} sr-only-like), {} still painted ({} sr-only-like); text boxes hidden by a clip {} sr-only-like + {} off-page + {} other, {} cut",
            self.clip_boxes,
            self.slivers_in_clip,
            self.slivers_in_sr,
            self.slivers_painted,
            self.slivers_sr_painted,
            self.text_hidden_sr,
            self.text_hidden_offpage,
            self.text_hidden_other.len(),
            self.text_cut
        )
    }

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
        let mut text = format!(
            "{}\n{}\npage width {page_width}\nwidest box: {}\n",
            self.headline(),
            self.clip_headline(),
            self.widest_label
        );
        let _ = writeln!(
            text,
            "\n## text slivers (first 60; clip = nearest ancestor that clips by style, sr: = sr-only-like; painted = what the clip table leaves)"
        );
        for line in self.slivers.iter().take(60) {
            let _ = writeln!(text, "{line}");
        }
        let _ = writeln!(
            text,
            "\n## on-page text hidden by a clip outside sr-only-like boxes — each needs a reason\n### by nearest clipping ancestor"
        );
        let mut by_clip: Vec<_> = self.text_hidden_other_by_clip.iter().collect();
        by_clip.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        for (clip, n) in by_clip {
            let _ = writeln!(text, "{n:5}  {clip}");
        }
        let _ = writeln!(
            text,
            "\n## observed elements clipped out (click refused: \"clipped out of view\")"
        );
        for line in &self.elements_clipped_out {
            let _ = writeln!(text, "{line}");
        }
        let _ = writeln!(text, "\n## text cut by a clip (first 100)");
        for line in &self.text_cut_lines {
            let _ = writeln!(text, "{line}");
        }
        let _ = writeln!(text, "### boxes (first 200)");
        for line in self.text_hidden_other.iter().take(200) {
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

fn walk_layout(layout: &LayoutBox, document: &Document, clips: &ClipTable, out: &mut Anomalies) {
    let mut walk = Walk {
        document,
        clips,
        out,
        path: Vec::new(),
    };
    walk.visit(layout, &ClipContext::default(), Affine::IDENTITY);
}

struct Walk<'a> {
    document: &'a Document,
    clips: &'a ClipTable,
    out: &'a mut Anomalies,
    path: Vec<String>,
}

/// The clipping ancestors of a box by style alone — a classification for
/// the probe, deliberately independent of the engine's containing-block
/// rules (those live in `layout::clip`, which this probe also reads).
#[derive(Clone, Default)]
struct ClipContext {
    /// The nearest ancestor that clips its content by style.
    nearest: Option<String>,
    /// The nearest sr-only-like ancestor (`ClipStyle::sr`).
    sr: Option<String>,
}

/// How an element clips its content by style: `overflow` other than
/// `visible` on either axis, `clip: rect()` on an absolute/fixed box, or
/// `clip-path: inset()`. The root `<html>` and `<body>` (their overflow goes
/// to the viewport) and `display: inline` boxes (overflow doesn't apply)
/// don't count. `sr` = the sr-only / visually-hidden shape: a `clip` or
/// `clip-path`, or overflow on a box at most 2px wide or tall.
struct ClipStyle {
    sr: bool,
    label: String,
}

fn clip_style(layout: &LayoutBox, node: &style::StyledNode) -> Option<ClipStyle> {
    let NodeType::Element(element) = &node.node_type else {
        return None;
    };
    let tag = element.tag_name.to_ascii_lowercase();
    if tag == "html" || tag == "body" {
        return None;
    }
    let keyword = |name: &str| match node.value(name) {
        Some(Value::Keyword(k)) => Some(k.as_str()),
        _ => None,
    };
    if keyword("display") == Some("inline") {
        return None;
    }
    let clips_axis = |name: &str| {
        keyword(name)
            .or_else(|| keyword("overflow"))
            .is_some_and(|k| k != "visible")
    };
    let overflow = clips_axis("overflow-x") || clips_axis("overflow-y");
    let positioned = matches!(keyword("position"), Some("absolute" | "fixed"));
    let clip_rect = positioned
        && matches!(node.value("clip"), Some(Value::ClipBox(b)) if b.function == css::ClipFunction::Rect);
    let clip_path = matches!(
        node.value("clip-path"),
        Some(Value::ClipBox(b)) if b.function == css::ClipFunction::Inset
    );
    if !(overflow || clip_rect || clip_path) {
        return None;
    }
    let d = &layout.dimensions;
    let width = d.content.width + d.padding.left + d.padding.right + d.border.left + d.border.right;
    let height =
        d.content.height + d.padding.top + d.padding.bottom + d.border.top + d.border.bottom;
    let tiny = width <= 2.0 || height <= 2.0;
    let mut why = Vec::new();
    if overflow {
        why.push(format!("overflow {width:.0}x{height:.0}"));
    }
    if clip_rect {
        why.push("clip:rect".into());
    }
    if clip_path {
        why.push("clip-path:inset".into());
    }
    Some(ClipStyle {
        sr: clip_rect || clip_path || (overflow && tiny),
        label: format!("{}[{}]", tag_label(element), why.join(" ")),
    })
}

fn tag_label(element: &mb_runtime::dom::ElementData) -> String {
    let mut label = element.tag_name.clone();
    if let Some(id_attr) = element.attributes.get("id") {
        let _ = write!(label, "#{id_attr}");
    }
    if let Some(class) = element.attributes.get("class") {
        for c in class.split_whitespace().take(2) {
            let _ = write!(label, ".{c}");
        }
    }
    label
}

/// `commands` with every `ClipGroup` replaced by its contents.
fn flatten_clips(commands: &[render::DisplayCommand]) -> Vec<render::DisplayCommand> {
    let mut out = Vec::with_capacity(commands.len());
    for command in commands {
        match command {
            render::DisplayCommand::ClipGroup(_, inner) => out.extend(flatten_clips(inner)),
            other => out.push(other.clone()),
        }
    }
    out
}

/// Shortest wall time of `runs` runs of `f`, in ms (a measurement column).
fn min_ms<T>(runs: usize, mut f: impl FnMut() -> T) -> f64 {
    (0..runs)
        .map(|_| {
            let started = Instant::now();
            std::hint::black_box(f());
            started.elapsed().as_secs_f64() * 1000.0
        })
        .fold(f64::INFINITY, f64::min)
}

/// Median wall time of three runs of `f`, in ms (a measurement column —
/// like `load_ms`, it varies run to run).
fn median_ms<T>(mut f: impl FnMut() -> T) -> f64 {
    let mut times: Vec<f64> = (0..3)
        .map(|_| {
            let started = Instant::now();
            std::hint::black_box(f());
            started.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    times.sort_by(f64::total_cmp);
    times[1]
}

/// `rect` under `matrix` as its axis-aligned bounding box (page space —
/// the space the clip table and the agent's click target use).
fn page_rect(rect: Rect, matrix: Affine) -> Rect {
    if matrix.is_identity() {
        return rect;
    }
    let corners = [
        (rect.x, rect.y),
        (rect.x + rect.width, rect.y),
        (rect.x, rect.y + rect.height),
        (rect.x + rect.width, rect.y + rect.height),
    ]
    .map(|(x, y)| matrix.apply_point(x, y));
    let min_x = corners.iter().map(|c| c.0).fold(f32::INFINITY, f32::min);
    let min_y = corners.iter().map(|c| c.1).fold(f32::INFINITY, f32::min);
    let max_x = corners
        .iter()
        .map(|c| c.0)
        .fold(f32::NEG_INFINITY, f32::max);
    let max_y = corners
        .iter()
        .map(|c| c.1)
        .fold(f32::NEG_INFINITY, f32::max);
    Rect {
        x: min_x,
        y: min_y,
        width: max_x - min_x,
        height: max_y - min_y,
    }
}

fn area(rect: Rect) -> f32 {
    rect.width.max(0.0) * rect.height.max(0.0)
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

impl Walk<'_> {
    /// `matrix` = the paint walk's inherited transform: clips are in page
    /// space after axis-aligned transforms, so a text box is compared in
    /// the same space (a closed drawer moved off-screen by `translateX`).
    fn visit(&mut self, layout: &LayoutBox, ctx: &ClipContext, matrix: Affine) {
        let matrix = matrix.compose(render::transform_for(layout));
        let out = &mut *self.out;
        out.boxes += 1;
        let rect = layout.dimensions.content;
        let styled = match &layout.box_type {
            BoxType::BlockNode(n)
            | BoxType::FlexNode(n)
            | BoxType::GridNode(n)
            | BoxType::TableNode(n) => Some(n),
            BoxType::AnonymousBlock => None,
        };
        if rect.width > out.widest {
            out.widest = rect.width;
            out.widest_label = format!(
                "{:.0}px {}  width={:?} min-width={:?}\n    layout path: {} < {}",
                rect.width,
                styled.map_or_else(
                    || "(anonymous)".to_string(),
                    |n| ancestry(self.document, n.node_id, 3)
                ),
                styled.and_then(|n| n.value("width")),
                styled.and_then(|n| n.value("min-width")),
                box_label(layout),
                self.path
                    .iter()
                    .rev()
                    .take(6)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" < ")
            );
        }
        // Step 3b: whether any box of each element survives its clip (the
        // agent's click target — page coordinates ∩ clip table).
        if let Some(node) = styled
            && matches!(node.node_type, NodeType::Element(_))
        {
            let d = &layout.dimensions;
            let border = Rect {
                x: d.content.x - d.padding.left - d.border.left,
                y: d.content.y - d.padding.top - d.border.top,
                width: d.content.width
                    + d.padding.left
                    + d.padding.right
                    + d.border.left
                    + d.border.right,
                height: d.content.height
                    + d.padding.top
                    + d.padding.bottom
                    + d.border.top
                    + d.border.bottom,
            };
            let visible = self
                .clips
                .visible_rect(node.node_id, page_rect(border, matrix))
                .is_some();
            *out.element_visible.entry(node.node_id).or_default() |= visible;
        }
        if let Some(node) = styled {
            if let NodeType::Text(text) = &node.node_type {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    self.text_box(node.node_id, rect, trimmed, ctx, matrix);
                }
            } else if let NodeType::Element(element) = &node.node_type
                && matches!(element.tag_name.as_str(), "style" | "script" | "template")
            {
                out.unrendered.push(format!(
                    "{} display={:?}  in {}\n    layout path: {}",
                    element.tag_name,
                    node.value("display"),
                    ancestry(self.document, node.node_id, 3),
                    self.path
                        .iter()
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
                    ancestry(self.document, node.node_id, 2)
                ));
            }
        }
        // What this box's own clip style means for its descendants.
        let inner = match styled.and_then(|node| clip_style(layout, node)) {
            Some(style) => ClipContext {
                sr: if style.sr {
                    Some(style.label.clone())
                } else {
                    ctx.sr.clone()
                },
                nearest: Some(style.label),
            },
            None => ctx.clone(),
        };
        self.path.push(box_label(layout));
        for child in &layout.children {
            self.visit(child, &inner, matrix);
        }
        self.path.pop();
    }

    fn text_box(
        &mut self,
        node: NodeId,
        rect: Rect,
        text: &str,
        ctx: &ClipContext,
        matrix: Affine,
    ) {
        let out = &mut *self.out;
        out.text_boxes += 1;
        // Under a rotation the clip table adds no clip of its own; compare
        // the untransformed box (only clips from above the rotation apply).
        let page_rect = if matrix.is_axis_aligned() {
            page_rect(rect, matrix)
        } else {
            rect
        };
        let visible = self.clips.visible_rect(node, page_rect);
        let sliver = text.chars().count() > 2 && rect.width / (text.chars().count() as f32) < 3.0;
        if sliver {
            out.slivers_in_clip += usize::from(ctx.nearest.is_some());
            out.slivers_in_sr += usize::from(ctx.sr.is_some());
            if visible.is_some() {
                out.slivers_painted += 1;
                out.slivers_sr_painted += usize::from(ctx.sr.is_some());
            }
            out.slivers.push(format!(
                "{:7.1}x{:<7.1} at ({:.0},{:.0})  {:?}  in {}  clip={}  painted={}",
                rect.width,
                rect.height,
                rect.x,
                rect.y,
                clip(text, 24),
                ancestry(self.document, node, 3),
                match (&ctx.sr, &ctx.nearest) {
                    (Some(sr), _) => format!("sr:{sr}"),
                    (None, Some(nearest)) => nearest.clone(),
                    (None, None) => "-".into(),
                },
                visible
                    .map(|v| format!("{:.0}x{:.0}", v.width, v.height))
                    .unwrap_or_else(|| "none".into())
            ));
        }
        if area(page_rect) <= 0.0 {
            return;
        }
        let off_page = page_rect.x >= out.page_width || page_rect.x + page_rect.width <= 0.0;
        match visible {
            None if ctx.sr.is_some() => out.text_hidden_sr += 1,
            None if off_page => out.text_hidden_offpage += 1,
            None => {
                *out.text_hidden_other_by_clip
                    .entry(
                        ctx.nearest
                            .clone()
                            .unwrap_or_else(|| "- (positioned ancestor rule)".into()),
                    )
                    .or_default() += 1;
                out.text_hidden_other.push(format!(
                    "{:.0}x{:.0} at ({:.0},{:.0})  {:?}  in {}  clip={}  clip rect={}",
                    rect.width,
                    rect.height,
                    rect.x,
                    rect.y,
                    clip(text, 40),
                    ancestry(self.document, node, 3),
                    ctx.nearest
                        .as_deref()
                        .unwrap_or("- (positioned ancestor rule)"),
                    self.clips
                        .clip_of(node)
                        .map(|c| format!(
                            "{:.0}x{:.0} at ({:.0},{:.0})",
                            c.width, c.height, c.x, c.y
                        ))
                        .unwrap_or_default()
                ));
            }
            // More than a pixel lost — sub-pixel rounding at a clip edge
            // isn't a cut.
            Some(v) if area(page_rect) - area(v) > 1.0 => {
                out.text_cut += 1;
                if out.text_cut_lines.len() < 100 {
                    out.text_cut_lines.push(format!(
                        "{:.0}x{:.0} at ({:.0},{:.0}) shows {:.0}x{:.0}  {:?}  in {}  clip={}",
                        page_rect.width,
                        page_rect.height,
                        page_rect.x,
                        page_rect.y,
                        v.width,
                        v.height,
                        clip(text, 30),
                        ancestry(self.document, node, 2),
                        ctx.nearest.as_deref().unwrap_or("-")
                    ));
                }
            }
            Some(_) => {}
        }
    }
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
