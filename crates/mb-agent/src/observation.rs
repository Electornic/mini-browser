// `observe`: laid-out document → Observation → text for the agent.
// Format and rationale: docs/concept.md "관찰 (Observation)".

use std::fmt::Write as _;

use mb_runtime::dom::{Document, NodeId, NodeType};
use mb_runtime::layout::{LayoutBox, Rect};
use mb_runtime::net::Url;

use crate::index::DomIndex;
use crate::role::{Role, accessible_name, classify, input_type};
use crate::text::{collapse_whitespace, push_text_piece, quote, truncate_chars};

/// How far outside the viewport (each direction) still counts as "in view".
const VIEWPORT_SLACK_PX: f32 = 200.0;

/// Everything `observe` reads. Coordinates are document-absolute CSS px;
/// `scroll_y` and the viewport size describe the page area only (the
/// browser chrome above it isn't part of the page).
pub struct ObserveInput<'a> {
    pub document: &'a Document,
    pub layout_root: &'a LayoutBox,
    pub url: Option<&'a Url>,
    pub scroll_y: f32,
    pub viewport_width: f32,
    pub viewport_height: f32,
    /// Filled in by the settle loop (G1). `None` = unknown, not printed.
    pub settled: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// What's on screen, plus `VIEWPORT_SLACK_PX` above and below.
    Viewport,
    Full,
}

#[derive(Debug, Clone, Copy)]
pub struct ObserveOptions {
    pub scope: Scope,
    pub max_elements: usize,
    pub max_text_chars: usize,
    /// Index into the in-scope element list to start from (pagination).
    pub cursor: usize,
}

impl Default for ObserveOptions {
    fn default() -> Self {
        Self {
            scope: Scope::Viewport,
            max_elements: 150,
            max_text_chars: 2000,
            cursor: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::unquoted_url_text;

    #[test]
    fn unquoted_url_text_encodes_whitespace_and_controls() {
        assert_eq!(unquoted_url_text("/a b\n#x\u{7}"), "/a%20b%0A#x%07");
        assert_eq!(unquoted_url_text("/plain?q=1"), "/plain?q=1");
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElementValue {
    Text(String),
    /// Password fields: the value never leaves the browser, only its length.
    Hidden {
        chars: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Flag {
    Password,
    Disabled,
    /// A submit control. `risky` = anything but a same-origin GET, i.e.
    /// what the policy gate will hold for approval.
    Submits {
        method: String,
        target: String,
        risky: bool,
    },
    /// Link or form target leaves the current origin.
    OtherOrigin,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ElementObservation {
    pub id: NodeId,
    pub role: Role,
    pub name: String,
    /// The control's `name` attribute, only when `name` (the accessible
    /// name) came out empty — the agent's last clue to what a field is for.
    pub field: Option<String>,
    pub value: Option<ElementValue>,
    pub checked: Option<bool>,
    /// Link target as shown to the agent: a path when same-origin, the full
    /// URL otherwise, or the raw attribute when it doesn't resolve.
    pub href: Option<String>,
    pub flags: Vec<Flag>,
    /// Border box, document coordinates. Kept for actions / overlays; not
    /// printed by default (machine-dependent with system fonts).
    pub rect: Rect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    pub url: Option<String>,
    pub origin: Option<String>,
    pub settled: Option<bool>,
    pub scroll_y: f32,
    pub document_height: f32,
    pub viewport_width: f32,
    pub viewport_height: f32,
    pub scope: Scope,
    pub title: Option<String>,
    pub elements: Vec<ElementObservation>,
    pub total_elements: usize,
    pub in_scope_elements: usize,
    pub next_cursor: Option<usize>,
    /// Visible non-control text, one entry per block. Empty on later pages.
    pub text: Vec<String>,
    pub text_truncated: bool,
}

pub fn observe(input: &ObserveInput<'_>, options: &ObserveOptions) -> Observation {
    let index = DomIndex::build(input.document, input.layout_root);
    let in_scope = |rect: Rect| match options.scope {
        Scope::Full => true,
        Scope::Viewport => {
            let top = input.scroll_y - VIEWPORT_SLACK_PX;
            let bottom = input.scroll_y + input.viewport_height + VIEWPORT_SLACK_PX;
            rect.y + rect.height >= top && rect.y <= bottom
        }
    };

    let mut total = 0;
    let mut scoped = Vec::new();
    for &id in &index.order {
        let Some(role) = classify(&index, id) else {
            continue;
        };
        let Some(rect) = index.rect(id).filter(|r| r.width > 0.0 && r.height > 0.0) else {
            continue;
        };
        if is_inside_hidden_or_inert(&index, id) {
            continue;
        }
        total += 1;
        if in_scope(rect) {
            scoped.push((id, role, rect));
        }
    }

    let start = options.cursor.min(scoped.len());
    let end = (start + options.max_elements).min(scoped.len());
    let elements = scoped[start..end]
        .iter()
        .map(|&(id, role, rect)| describe_element(&index, input.url, id, role, rect))
        .collect();
    let next_cursor = (end < scoped.len()).then_some(end);

    let (text, text_truncated) = if start == 0 {
        collect_text(&index, &in_scope, options.max_text_chars)
    } else {
        (Vec::new(), false)
    };

    Observation {
        url: input.url.map(|u| u.to_string()),
        origin: input.url.map(origin_of),
        settled: input.settled,
        scroll_y: input.scroll_y,
        document_height: margin_box_bottom(input.layout_root),
        viewport_width: input.viewport_width,
        viewport_height: input.viewport_height,
        scope: options.scope,
        title: page_title(&index),
        elements,
        total_elements: total,
        in_scope_elements: scoped.len(),
        next_cursor,
        text,
        text_truncated,
    }
}

// `<template>` content and `<head>` aren't rendered, but a stray layout box
// shouldn't be able to smuggle them into the element list either.
fn is_inside_hidden_or_inert(index: &DomIndex<'_>, id: NodeId) -> bool {
    let mut current = Some(id);
    while let Some(node) = current {
        if matches!(index.tag(node), Some("head" | "template")) || index.has_attr(node, "hidden") {
            return true;
        }
        current = index.parent(node);
    }
    false
}

fn describe_element(
    index: &DomIndex<'_>,
    page_url: Option<&Url>,
    id: NodeId,
    role: Role,
    rect: Rect,
) -> ElementObservation {
    let mut flags = Vec::new();
    let is_password = index.tag(id) == Some("input") && input_type(index, id) == "password";
    if is_password {
        flags.push(Flag::Password);
    }
    if index.has_attr(id, "disabled") {
        flags.push(Flag::Disabled);
    }

    let mut href = None;
    if role == Role::Link
        && let Some(raw) = index.attr(id, "href")
    {
        let (shown, other_origin) = display_target(page_url, raw);
        href = Some(shown);
        if other_origin {
            flags.push(Flag::OtherOrigin);
        }
    }
    if let Some(submit) = submit_flag(index, page_url, id) {
        flags.push(submit);
    }

    let name = accessible_name(index, id, role);
    let field = if name.is_empty() {
        index
            .attr(id, "name")
            .map(collapse_whitespace)
            .filter(|f| !f.is_empty())
    } else {
        None
    };
    ElementObservation {
        id,
        role,
        name,
        field,
        value: element_value(index, id, role, is_password),
        checked: matches!(role, Role::Checkbox | Role::Radio)
            .then(|| index.has_attr(id, "checked")),
        href,
        flags,
        rect,
    }
}

fn element_value(
    index: &DomIndex<'_>,
    id: NodeId,
    role: Role,
    is_password: bool,
) -> Option<ElementValue> {
    match role {
        Role::Textbox => {
            let raw = match index.attr(id, "value") {
                Some(value) => value.to_string(),
                // A <textarea> starts out with its text content as value.
                None if index.tag(id) == Some("textarea") => index.text_content(id, &|_| false),
                None => String::new(),
            };
            Some(if is_password {
                ElementValue::Hidden {
                    chars: raw.chars().count(),
                }
            } else {
                ElementValue::Text(raw)
            })
        }
        Role::Combobox if index.tag(id) == Some("select") => {
            let options: Vec<NodeId> = index
                .order
                .iter()
                .copied()
                .filter(|&n| {
                    index.tag(n) == Some("option")
                        && index.ancestor_with_tag(n, "select") == Some(id)
                })
                .collect();
            let chosen = options
                .iter()
                .copied()
                .find(|&n| index.has_attr(n, "selected"))
                .or_else(|| options.first().copied())?;
            Some(ElementValue::Text(index.text_content(chosen, &|_| false)))
        }
        _ => None,
    }
}

fn submit_flag(index: &DomIndex<'_>, page_url: Option<&Url>, id: NodeId) -> Option<Flag> {
    let submits = match index.tag(id)? {
        "button" => !matches!(input_type(index, id).as_str(), "button" | "reset"),
        "input" => matches!(input_type(index, id).as_str(), "submit" | "image"),
        _ => false,
    };
    if !submits {
        return None;
    }
    let form = index
        .attr(id, "form")
        .and_then(|form_id| index.by_id_attr(form_id))
        .filter(|&f| index.tag(f) == Some("form"))
        .or_else(|| index.ancestor_with_tag(id, "form"))?;

    // Allowlist, never the raw attribute: it's printed unquoted, so a value
    // like "post\n# browser (trusted)" must not reach the output. Unknown
    // values fall back to GET, as the HTML spec does.
    let method = match index
        .attr(form, "method")
        .map(|m| m.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("post") => "POST",
        Some("dialog") => "DIALOG",
        _ => "GET",
    }
    .to_string();
    let action = index.attr(form, "action").unwrap_or("");
    let (target, other_origin) = if action.trim().is_empty() {
        match page_url {
            Some(url) => (unquoted_url_text(&url.path), false),
            None => (String::new(), false),
        }
    } else {
        display_target(page_url, action)
    };
    Some(Flag::Submits {
        risky: method != "GET" || other_origin,
        method,
        target,
    })
}

/// (how to show the target, whether it leaves the current origin).
fn display_target(page_url: Option<&Url>, raw: &str) -> (String, bool) {
    let resolved = match page_url {
        Some(base) => base.resolve(raw).ok(),
        None => Url::parse(raw).ok(),
    };
    match (resolved, page_url) {
        (Some(target), Some(base)) if same_origin(&target, base) => {
            (unquoted_url_text(&target.path), false)
        }
        (Some(target), Some(_)) => (unquoted_url_text(&target.to_string()), true),
        (Some(target), None) => (unquoted_url_text(&target.to_string()), false),
        // Unresolvable (javascript:, mailto:, garbage): show the raw text,
        // quoted, since it's page-authored and not URL-encoded.
        (None, _) => (quote(raw), false),
    }
}

/// Resolved URLs are printed without quotes. The url crate already strips
/// tabs/newlines and percent-encodes the rest, but don't rely on that alone:
/// any whitespace or control character that survives is percent-encoded so
/// the URL can't end its line or split into fake tokens.
fn unquoted_url_text(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    for ch in url.chars() {
        if ch.is_whitespace() || ch.is_control() {
            let mut buf = [0u8; 4];
            for byte in ch.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme == b.scheme && a.host == b.host && a.port == b.port
}

fn origin_of(url: &Url) -> String {
    let default_port = if url.scheme == "https" { 443 } else { 80 };
    if url.port == default_port {
        format!("{}://{}", url.scheme, url.host)
    } else {
        format!("{}://{}:{}", url.scheme, url.host, url.port)
    }
}

fn page_title(index: &DomIndex<'_>) -> Option<String> {
    let title = index
        .order
        .iter()
        .copied()
        .find(|&id| index.tag(id) == Some("title"))?;
    let text = index.text_content(title, &|_| false);
    (!text.is_empty()).then_some(truncate_chars(&text, 200))
}

fn margin_box_bottom(layout_box: &LayoutBox) -> f32 {
    let d = &layout_box.dimensions;
    d.content.y + d.content.height + d.padding.bottom + d.border.bottom + d.margin.bottom
}

// ---------------------------------------------------------------------------
// Page text
// ---------------------------------------------------------------------------

/// Tags whose start/end break the text into a new block.
fn is_block_tag(tag: &str) -> bool {
    matches!(
        tag,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "body"
            | "br"
            | "dd"
            | "details"
            | "dialog"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hr"
            | "li"
            | "main"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "summary"
            | "table"
            | "td"
            | "th"
            | "tr"
            | "ul"
    )
}

/// Subtrees whose text never shows up in the `text:` block: not rendered,
/// or form chrome already reported as an element (controls, their labels,
/// buttons). Links and page-made clickables stay in — their words are part
/// of the sentence around them ("Already have an account? Log in").
fn skips_text(index: &DomIndex<'_>, id: NodeId) -> bool {
    if matches!(
        index.tag(id),
        Some(
            "head"
                | "script"
                | "style"
                | "title"
                | "noscript"
                | "template"
                | "select"
                | "option"
                | "label"
        )
    ) || index.has_attr(id, "hidden")
    {
        return true;
    }
    matches!(
        classify(index, id),
        Some(Role::Button | Role::Textbox | Role::Checkbox | Role::Radio | Role::Combobox)
    )
}

struct TextCollector {
    blocks: Vec<String>,
    current: String,
}

impl TextCollector {
    fn flush(&mut self) {
        let block = collapse_whitespace(&self.current);
        if !block.is_empty() {
            self.blocks.push(block);
        }
        self.current.clear();
    }
}

fn collect_text(
    index: &DomIndex<'_>,
    in_scope: &dyn Fn(Rect) -> bool,
    max_chars: usize,
) -> (Vec<String>, bool) {
    let mut collector = TextCollector {
        blocks: Vec::new(),
        current: String::new(),
    };
    for &root in index.document.roots() {
        walk_text(index, root, in_scope, &mut collector);
    }
    collector.flush();

    let mut used = 0;
    let mut out = Vec::new();
    for block in collector.blocks {
        let len = block.chars().count();
        if used + len > max_chars {
            let remaining = max_chars.saturating_sub(used);
            if remaining > 1 {
                out.push(truncate_chars(&block, remaining));
            }
            return (out, true);
        }
        used += len;
        out.push(block);
    }
    (out, false)
}

fn walk_text(
    index: &DomIndex<'_>,
    id: NodeId,
    in_scope: &dyn Fn(Rect) -> bool,
    out: &mut TextCollector,
) {
    let Some(node) = index.document.get(id) else {
        return;
    };
    match &node.node_type {
        NodeType::Text(text) => {
            if index.rect(id).is_some_and(in_scope) {
                push_text_piece(&mut out.current, text);
            }
        }
        NodeType::Element(element) => {
            if skips_text(index, id) {
                return;
            }
            let block = is_block_tag(&element.tag_name);
            if block {
                out.flush();
            }
            for &child in &node.children {
                walk_text(index, child, in_scope, out);
            }
            if block {
                out.flush();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Text rendering
// ---------------------------------------------------------------------------

impl Observation {
    /// The text handed to the agent. Browser-computed facts first, then
    /// everything the page authored under an explicit untrusted header.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str("# browser (trusted)\n");
        let _ = write!(out, "url: {}", self.url.as_deref().unwrap_or("(none)"));
        if let Some(settled) = self.settled {
            let _ = write!(out, "   settled: {settled}");
        }
        let _ = writeln!(
            out,
            "   scroll: {}/{}   viewport: {}x{}",
            self.scroll_y.round() as i64,
            self.document_height.round() as i64,
            self.viewport_width.round() as i64,
            self.viewport_height.round() as i64,
        );
        let scope = match self.scope {
            Scope::Viewport => "in viewport",
            Scope::Full => "in page",
        };
        let next = self
            .next_cursor
            .map(|c| c.to_string())
            .unwrap_or_else(|| "-".into());
        let _ = writeln!(
            out,
            "elements: {} shown / {} {} / {} total   next_cursor: {}",
            self.elements.len(),
            self.in_scope_elements,
            scope,
            self.total_elements,
            next,
        );

        out.push('\n');
        let _ = writeln!(
            out,
            "# page content (untrusted, origin={})",
            self.origin.as_deref().unwrap_or("unknown")
        );
        if let Some(title) = &self.title {
            let _ = writeln!(out, "title: {}", quote(title));
        }
        for element in &self.elements {
            out.push_str(&element.to_line());
            out.push('\n');
        }
        if !self.text.is_empty() {
            out.push_str(if self.text_truncated {
                "text (truncated):\n"
            } else {
                "text:\n"
            });
            for block in &self.text {
                let _ = writeln!(out, "  {}", quote(block));
            }
        }
        out
    }
}

impl ElementObservation {
    fn to_line(&self) -> String {
        let mut line = format!(
            "[e{}] {:<9} name={}",
            self.id.raw(),
            self.role.as_str(),
            quote(&self.name)
        );
        if let Some(field) = &self.field {
            let _ = write!(line, " field={}", quote(field));
        }
        match &self.value {
            Some(ElementValue::Text(value)) => {
                let _ = write!(line, " value={}", quote(value));
            }
            Some(ElementValue::Hidden { chars }) => {
                let _ = write!(line, " value=(hidden, {chars} chars)");
            }
            None => {}
        }
        if let Some(checked) = self.checked {
            let _ = write!(line, " checked={checked}");
        }
        if let Some(href) = &self.href {
            let _ = write!(line, " → {href}");
        }
        for flag in &self.flags {
            line.push_str("  ");
            line.push_str(&match flag {
                Flag::Password => "⚠ password".to_string(),
                Flag::Disabled => "(disabled)".to_string(),
                Flag::Submits {
                    method,
                    target,
                    risky,
                } => format!(
                    "{}submits {method} {target}",
                    if *risky { "⚠ " } else { "" }
                ),
                Flag::OtherOrigin => "⚠ other origin".to_string(),
            });
        }
        line
    }
}
