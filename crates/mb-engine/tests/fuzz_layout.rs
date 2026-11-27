//! Structured layout fuzzer + CSS parser token fuzzer — compat round 3 step
//! 0c (docs/compat-round3.md "0.3 퍼징").
//!
//! No new dependencies and no cargo-fuzz (the toolchain is pinned to stable):
//! a seeded xorshift builds HTML + CSS *structurally* (a tree of elements and
//! a list of rules), runs the whole engine pipeline
//! `html::parse → css::parse → style_tree → layout_tree(800) → build_document_view`
//! and judges the result. Everything is deterministic for a given seed.
//!
//! Verdicts (both modes): a panic, a NaN/infinite layout coordinate, a box
//! wider than 1e7 px (an *amplification* — the 2nd-round GitHub 187M px box;
//! only judged on cases whose own values are bounded, see [`Values`]), and
//! a per-case cap. The default `cargo test` run (200 seeds) caps a case at 5 s
//! (debug build, parallel tests — only catches runaway loops); the `#[ignore]`d
//! long run also fails any case over 1 s:
//!
//! ```sh
//! MB_FUZZ_CASES=100000 cargo test --release -p mb-engine --test fuzz_layout -- --ignored --nocapture
//! ```
//!
//! `MB_FUZZ_SEED` moves the first seed (default 1), `MB_FUZZ_FONTS=1` installs
//! the macOS system fonts so text goes through cosmic-text shaping instead of
//! the toy width estimate (slower; skipped silently if the files are missing).
//!
//! On a failure the case is **shrunk** — remove one child / one declaration /
//! one rule at a time while the same kind of failure still reproduces — and
//! the seed plus the shrunk HTML/CSS is printed. Turn that into a hand-cleaned
//! unit test in the owning module, named after the symptom; never paste
//! fuzzer seeds into tests (they lose their meaning when the generator
//! changes).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Once, mpsc};
use std::time::{Duration, Instant};

use mb_dom::{css, html, style};
use mb_engine::layout::{self, LayoutBox};
use mb_engine::render::DisplayCommand;
use mb_engine::view;

// ---------------------------------------------------------------- rng ----

/// xorshift64 seeded through splitmix64 (so seeds 1, 2, 3… don't start in
/// correlated states and seed 0 is not the stuck all-zero state).
#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        Rng(z | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform-ish in `0..n` (`n > 0`).
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// Inclusive range.
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    fn pick_str(&mut self, items: &[&'static str]) -> &'static str {
        items[self.below(items.len())]
    }
}

// ---------------------------------------------------------- the model ----

#[derive(Clone, Debug)]
enum Node {
    Element(Element),
    Text(String),
}

#[derive(Clone, Debug)]
struct Element {
    tag: &'static str,
    /// Plain attributes (`class`, `hidden`, `colspan`, …) — fixed once made.
    attrs: Vec<(&'static str, String)>,
    /// `style=""` declarations, shrinkable one by one.
    style: Vec<String>,
    children: Vec<Node>,
}

#[derive(Clone, Debug)]
struct Rule {
    selector: String,
    decls: Vec<String>,
}

#[derive(Clone, Debug)]
struct Case {
    body: Vec<Node>,
    rules: Vec<Rule>,
}

const VOID_TAGS: &[&str] = &["input", "img", "br"];

impl Case {
    fn html(&self) -> String {
        let mut out = String::from("<!DOCTYPE html><html><head></head><body>");
        for node in &self.body {
            write_node(&mut out, node, 0);
        }
        out.push_str("</body></html>");
        out
    }

    fn css(&self) -> String {
        let mut out = String::new();
        for rule in &self.rules {
            let _ = writeln!(out, "{} {{ {} }}", rule.selector, rule.decls.join("; "));
        }
        out
    }

    /// Whether any value in the case is from the unbounded pool — those
    /// legitimately make enormous boxes, so the 1e7 px amplification verdict
    /// is not judged on them (NaN/inf, panics and time still are).
    fn has_unbounded_values(&self) -> bool {
        let unbounded = |decl: &String| UNBOUNDED_MARKERS.iter().any(|m| decl.contains(m));
        self.rules.iter().any(|r| r.decls.iter().any(unbounded))
            || self.body.iter().any(|n| node_any_style(n, &unbounded))
    }
}

fn node_any_style(node: &Node, pred: &dyn Fn(&String) -> bool) -> bool {
    match node {
        Node::Text(_) => false,
        Node::Element(el) => {
            el.style.iter().any(pred) || el.children.iter().any(|c| node_any_style(c, pred))
        }
    }
}

fn write_node(out: &mut String, node: &Node, depth: usize) {
    match node {
        Node::Text(text) => escape_into(out, text, false),
        Node::Element(el) => {
            // Pretty enough to read a shrunk case: one element per line.
            if depth > 0 {
                out.push('\n');
                out.extend(std::iter::repeat_n(' ', depth.min(40)));
            }
            out.push('<');
            out.push_str(el.tag);
            for (name, value) in &el.attrs {
                out.push(' ');
                out.push_str(name);
                if !value.is_empty() {
                    out.push_str("=\"");
                    escape_into(out, value, true);
                    out.push('"');
                }
            }
            if !el.style.is_empty() {
                out.push_str(" style=\"");
                escape_into(out, &el.style.join("; "), true);
                out.push('"');
            }
            out.push('>');
            if VOID_TAGS.contains(&el.tag) {
                return;
            }
            for child in &el.children {
                write_node(out, child, depth + 1);
            }
            let _ = write!(out, "</{}>", el.tag);
        }
    }
}

fn escape_into(out: &mut String, text: &str, attr: bool) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' if attr => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
}

// ---------------------------------------------------------- generator ----

const TAGS: &[&str] = &[
    "div", "span", "p", "a", "button", "input", "table", "tr", "td", "ul", "li", "img", "label",
    "form",
];
const CLASSES: &[&str] = &["a", "b", "c", "d", "e"];
const AREA_NAMES: &[&str] = &["a", "b", "c", "."];

/// Words for text nodes: Korean, mixed spaces, long unbreakable words,
/// punctuation, zero-width / no-break spaces, an emoji and a combining mark.
const WORDS: &[&str] = &[
    "hello",
    "world",
    "안녕하세요",
    "가나다라마바사아자차카타파하",
    "한",
    " ",
    "   ",
    "\t",
    "\n",
    "\u{a0}",
    "\u{200b}",
    "—",
    ",",
    ".",
    "😀",
    "e\u{301}",
    "supercalifragilisticexpialidocious",
    "https://example.test/a/very/long/path/without/any/break/opportunities",
];

/// Which value pool a case draws from. Most cases are **bounded**: every
/// length, percentage and font-size is small enough that no legitimate
/// layout of ≤ 40 levels reaches 1e7 px, so a wider box is an engine
/// amplification bug. A minority are **unbounded** (1e9 px, 1e30 px, 1e39 px
/// = f32 overflow, 1000 % font sizes compounding, 100 000 grid tracks): they
/// probe NaN/inf arithmetic (D3) and allocation limits, and skip only the
/// width verdict.
#[derive(Clone, Copy, PartialEq)]
enum Values {
    Bounded,
    Unbounded,
}

/// Every unbounded value contains one of these, so a shrunk case can be
/// re-classified from its text alone.
const UNBOUNDED_MARKERS: &[&str] = &[
    "e9",
    "e30",
    "e38",
    "e39",
    "1000%",
    "100000",
    "000000000",
    "20em",
    "-60%",
    "-100%",
    "99999px",
    "20000px",
];

struct Gen {
    rng: Rng,
    values: Values,
    budget: usize,
    max_depth: usize,
}

impl Gen {
    fn case(seed: u64) -> Case {
        let mut rng = Rng::new(seed);
        let values = if rng.chance(20) {
            Values::Unbounded
        } else {
            Values::Bounded
        };
        let budget = if rng.chance(10) {
            rng.range(1, 400)
        } else {
            rng.range(1, 120)
        };
        let max_depth = rng.range(0, 40);
        let mut generator = Gen {
            rng,
            values,
            budget,
            max_depth,
        };
        let body = generator.children(0);
        let rule_count = generator.rng.range(0, 12);
        let rules = (0..rule_count).map(|_| generator.rule()).collect();
        Case { body, rules }
    }

    fn children(&mut self, depth: usize) -> Vec<Node> {
        let mut out = Vec::new();
        if depth > self.max_depth {
            return out;
        }
        let count = if depth == 0 {
            self.rng.range(1, 4)
        } else {
            self.rng.range(0, 4)
        };
        // One child of each element is a "spine" that keeps descending with
        // high probability, so deep trees (up to `max_depth`) actually occur.
        // The spine is not limited by the node budget (it adds at most
        // `max_depth` nodes); siblings are.
        let spine = self.rng.below(count.max(1));
        for i in 0..count {
            let is_spine = i == spine && self.rng.chance(97);
            if !is_spine {
                if self.budget == 0 {
                    continue;
                }
                self.budget -= 1;
            }
            if !is_spine && self.rng.chance(25) {
                out.push(Node::Text(self.text()));
            } else {
                out.push(Node::Element(self.element(depth, is_spine)));
            }
        }
        out
    }

    fn element(&mut self, depth: usize, descend: bool) -> Element {
        let tag = *self.rng.pick(TAGS);
        let mut attrs = Vec::new();
        if self.rng.chance(40) {
            let class = if self.rng.chance(20) {
                format!("{} {}", self.rng.pick(CLASSES), self.rng.pick(CLASSES))
            } else {
                self.rng.pick(CLASSES).to_string()
            };
            attrs.push(("class", class));
        }
        if self.rng.chance(5) {
            attrs.push(("hidden", String::new()));
        }
        match tag {
            "td" if self.rng.chance(20) => {
                let span = *self.rng.pick(&["0", "1", "2", "3", "-1", "1000", "99999"]);
                attrs.push((*self.rng.pick(&["colspan", "rowspan"]), span.to_string()));
            }
            "img" => {
                if self.rng.chance(50) {
                    let size = *self.rng.pick(&["0", "1", "50", "-3", "100%", "abc"]);
                    attrs.push((*self.rng.pick(&["width", "height"]), size.to_string()));
                }
                if self.rng.chance(30) {
                    attrs.push(("alt", self.text()));
                }
            }
            "input" => {
                let kind = *self
                    .rng
                    .pick(&["text", "checkbox", "radio", "submit", "password", "hidden"]);
                attrs.push(("type", kind.to_string()));
                if self.rng.chance(40) {
                    attrs.push(("value", self.text()));
                }
            }
            "a" if self.rng.chance(70) => attrs.push(("href", "#x".to_string())),
            _ => {}
        }
        let style = if self.rng.chance(20) {
            let n = self.rng.range(1, 4);
            (0..n).map(|_| self.declaration()).collect()
        } else {
            Vec::new()
        };
        let children = if VOID_TAGS.contains(&tag) {
            Vec::new()
        } else if descend {
            // Spine: always at least one more level until `max_depth`.
            let mut kids = self.children(depth + 1);
            if kids.is_empty() && depth < self.max_depth {
                kids.push(Node::Element(self.element(depth + 1, true)));
            }
            kids
        } else if self.rng.chance(40) {
            self.children(depth + 1)
        } else {
            Vec::new()
        };
        Element {
            tag,
            attrs,
            style,
            children,
        }
    }

    fn text(&mut self) -> String {
        let target = self.rng.range(0, 200);
        let mut out = String::new();
        while out.chars().count() < target {
            if self.rng.chance(5) {
                // A long word with no break opportunity.
                let n = self.rng.range(20, 150);
                out.extend(std::iter::repeat_n('w', n));
            } else {
                out.push_str(self.rng.pick_str(WORDS));
            }
            if self.rng.chance(60) {
                out.push(' ');
            }
        }
        out.chars().take(target).collect()
    }

    fn selector(&mut self) -> String {
        let simple = |g: &mut Gen| -> String {
            match g.rng.below(4) {
                0 => g.rng.pick(TAGS).to_string(),
                1 | 2 => format!(".{}", g.rng.pick(CLASSES)),
                _ => format!("{}.{}", g.rng.pick(TAGS), g.rng.pick(CLASSES)),
            }
        };
        match self.rng.below(6) {
            0 => "*".to_string(),
            1 | 2 => simple(self),
            3 => format!("{} {}", simple(self), simple(self)),
            4 => format!("{} > {}", simple(self), simple(self)),
            _ => format!("{} {} {}", simple(self), simple(self), simple(self)),
        }
    }

    fn rule(&mut self) -> Rule {
        let selector = self.selector();
        let n = self.rng.range(1, 8);
        let decls = (0..n).map(|_| self.declaration()).collect();
        Rule { selector, decls }
    }

    // ------------------------------------------------------- values ----

    fn length(&mut self) -> String {
        if self.values == Values::Unbounded && self.rng.chance(30) {
            return self
                .rng
                .pick(&[
                    "1e9px", "-1e9px", "1e30px", "3e38px", "1e39px", "-1e39px", "1000%", "1e30%",
                    "1e9em", "-60%", "-100%", "99999px", "20000px",
                ])
                .to_string();
        }
        match self.rng.below(12) {
            0 => "0".to_string(),
            1 => "auto".to_string(),
            2 => format!("-{}px", self.rng.range(1, 2000)),
            3 => format!("{}%", self.rng.range(0, 120)),
            // Small negative percentages only: nested `margin: -50%` legitimately
            // doubles the width per level (Chrome too) — that is unbounded.
            4 => format!("-{}%", self.rng.range(1, 5)),
            5 => format!("{}.{}em", self.rng.range(0, 3), self.rng.range(0, 9)),
            6 => format!("{}rem", self.rng.range(0, 4)),
            7 => self
                .rng
                .pick(&[
                    "min-content",
                    "max-content",
                    "fit-content",
                    "0.0001px",
                    "0px",
                    "-0px",
                ])
                .to_string(),
            // ≤ 1500px: a nowrap line of ~800 boxes each with six such edges
            // stays under the 1e7 verdict (20 000px moved to the unbounded pool).
            8 => format!("{}px", self.rng.range(1000, 1500)),
            _ => format!("{}px", self.rng.range(0, 400)),
        }
    }

    fn number(&mut self) -> String {
        if self.values == Values::Unbounded && self.rng.chance(30) {
            return self
                .rng
                .pick(&["1e30", "1e39", "-1e39", "3e38"])
                .to_string();
        }
        self.rng
            .pick(&["0", "1", "2", "0.5", "-1", "10", "1000"])
            .to_string()
    }

    fn font_size(&mut self) -> String {
        if self.values == Values::Unbounded && self.rng.chance(40) {
            return self
                .rng
                .pick(&["1000%", "1e9px", "1e39px", "20em", "-1e9px"])
                .to_string();
        }
        match self.rng.below(6) {
            0 => format!("{}px", self.rng.range(0, 64)),
            1 => format!("-{}px", self.rng.range(1, 20)),
            // Relative sizes never grow: `* { font-size: 110% }` compounds
            // over 40 levels, and every em edge with it.
            2 => format!("{}%", self.rng.range(50, 100)),
            3 => "1em".to_string(),
            4 => format!("0.{}em", self.rng.range(0, 9)),
            _ => self
                .rng
                .pick(&["0", "small", "larger", "smaller", "xx-large", "medium"])
                .to_string(),
        }
    }

    fn track(&mut self) -> String {
        match self.rng.below(9) {
            0 => format!("{}fr", self.rng.range(0, 3)),
            1 => "auto".to_string(),
            2 => self.rng.pick(&["min-content", "max-content"]).to_string(),
            3 => format!("minmax({}, {})", self.length(), self.track_simple()),
            4 => {
                let count = if self.values == Values::Unbounded && self.rng.chance(30) {
                    "100000".to_string()
                } else {
                    self.rng.pick(&["0", "1", "3", "1000"]).to_string()
                };
                format!("repeat({count}, {})", self.track_simple())
            }
            5 => self
                .rng
                .pick(&[
                    "-1fr",
                    "-10px",
                    "fit-content(100px)",
                    "repeat(auto-fill, 50px)",
                ])
                .to_string(),
            _ => self.length(),
        }
    }

    fn track_simple(&mut self) -> String {
        match self.rng.below(3) {
            0 => format!("{}fr", self.rng.range(0, 3)),
            1 => "auto".to_string(),
            _ => self.length(),
        }
    }

    fn grid_line(&mut self) -> String {
        let one = |g: &mut Gen| -> String {
            if g.values == Values::Unbounded && g.rng.chance(20) {
                return g
                    .rng
                    .pick(&["2000000000", "span 1000000000", "-2000000000"])
                    .to_string();
            }
            match g.rng.below(6) {
                0 => "auto".to_string(),
                1 => format!("span {}", g.rng.range(0, 5)),
                2 => format!("-{}", g.rng.range(1, 5)),
                3 => g.rng.pick(&["a", "b", "c", "span a"]).to_string(),
                _ => g.rng.range(0, 6).to_string(),
            }
        };
        if self.rng.chance(50) {
            format!("{} / {}", one(self), one(self))
        } else {
            one(self)
        }
    }

    fn areas(&mut self) -> String {
        // Rows of inconsistent widths on purpose (area/track mismatch).
        let rows = self.rng.range(1, 4);
        let mut out = String::new();
        for _ in 0..rows {
            let cols = self.rng.range(0, 4);
            let names: Vec<&str> = (0..cols).map(|_| *self.rng.pick(AREA_NAMES)).collect();
            let _ = write!(out, "\"{}\" ", names.join(" "));
        }
        out.trim_end().to_string()
    }

    fn declaration(&mut self) -> String {
        const DISPLAY: &[&str] = &[
            "block",
            "inline",
            "inline-block",
            "flex",
            "inline-flex",
            "grid",
            "inline-grid",
            "table",
            "table-row",
            "table-cell",
            "table-row-group",
            "table-header-group",
            "list-item",
            "none",
            "contents",
            "flow-root",
        ];
        let (name, value): (&str, String) = match self.rng.below(32) {
            0..=3 => ("display", self.rng.pick(DISPLAY).to_string()),
            4 => (
                "position",
                self.rng
                    .pick(&["static", "relative", "absolute", "fixed", "sticky"])
                    .to_string(),
            ),
            5 => (
                *self.rng.pick(&["top", "left", "right", "bottom"]),
                self.length(),
            ),
            6 => (
                "float",
                self.rng.pick(&["left", "right", "none"]).to_string(),
            ),
            7 => (
                "clear",
                self.rng
                    .pick(&["left", "right", "both", "none"])
                    .to_string(),
            ),
            8 | 9 => (
                *self.rng.pick(&[
                    "width",
                    "height",
                    "min-width",
                    "max-width",
                    "min-height",
                    "max-height",
                ]),
                self.length(),
            ),
            10 | 11 => {
                let n = self.rng.range(1, 4);
                let parts: Vec<String> = (0..n).map(|_| self.length()).collect();
                (*self.rng.pick(&["margin", "padding"]), parts.join(" "))
            }
            12 => (
                *self.rng.pick(&[
                    "margin-top",
                    "margin-left",
                    "padding-right",
                    "padding-bottom",
                    "border-width",
                ]),
                self.length(),
            ),
            13 => ("border", format!("{} solid red", self.length())),
            14 => (
                "box-sizing",
                self.rng.pick(&["border-box", "content-box"]).to_string(),
            ),
            15 => {
                let flex = match self.rng.below(4) {
                    0 => self.number(),
                    1 => format!("{} {} {}", self.number(), self.number(), self.length()),
                    2 => self
                        .rng
                        .pick(&["none", "auto", "1 1 0%", "0 0 auto"])
                        .to_string(),
                    _ => format!("{} {}", self.number(), self.length()),
                };
                ("flex", flex)
            }
            16 => (*self.rng.pick(&["flex-grow", "flex-shrink"]), self.number()),
            17 => ("flex-basis", self.length()),
            18 => (
                *self.rng.pick(&["flex-direction", "flex-wrap"]),
                self.rng
                    .pick(&[
                        "row",
                        "column",
                        "row-reverse",
                        "column-reverse",
                        "wrap",
                        "nowrap",
                        "wrap-reverse",
                    ])
                    .to_string(),
            ),
            19 => (
                *self.rng.pick(&[
                    "justify-content",
                    "align-items",
                    "align-self",
                    "align-content",
                ]),
                self.rng
                    .pick(&[
                        "center",
                        "stretch",
                        "flex-start",
                        "flex-end",
                        "space-between",
                        "baseline",
                        "start",
                    ])
                    .to_string(),
            ),
            20 => (
                *self.rng.pick(&["gap", "row-gap", "column-gap"]),
                self.length(),
            ),
            21 | 22 => {
                let n = self.rng.range(0, 6);
                let tracks: Vec<String> = (0..n).map(|_| self.track()).collect();
                let value = if tracks.is_empty() {
                    "none".to_string()
                } else {
                    tracks.join(" ")
                };
                (
                    *self
                        .rng
                        .pick(&["grid-template-columns", "grid-template-rows"]),
                    value,
                )
            }
            23 => ("grid-template-areas", self.areas()),
            24 => (
                *self.rng.pick(&["grid-column", "grid-row"]),
                self.grid_line(),
            ),
            25 => (
                "grid-area",
                self.rng
                    .pick(&["a", "b", "c", "1 / 2 / 3 / 4", "auto"])
                    .to_string(),
            ),
            26 => (
                "grid-auto-flow",
                self.rng
                    .pick(&["row", "column", "dense", "column dense"])
                    .to_string(),
            ),
            27 => (
                *self.rng.pick(&["overflow", "overflow-x", "overflow-y"]),
                self.rng
                    .pick(&["visible", "hidden", "scroll", "auto", "clip"])
                    .to_string(),
            ),
            28 => (
                "white-space",
                self.rng
                    .pick(&[
                        "normal",
                        "nowrap",
                        "pre",
                        "pre-wrap",
                        "pre-line",
                        "break-spaces",
                    ])
                    .to_string(),
            ),
            29 => {
                let value = match self.rng.below(3) {
                    0 => self.number(),
                    1 => self.length(),
                    _ => "normal".to_string(),
                };
                ("line-height", value)
            }
            30 => ("font-size", self.font_size()),
            _ => (
                "text-align",
                self.rng
                    .pick(&["left", "right", "center", "justify"])
                    .to_string(),
            ),
        };
        format!("{name}: {value}")
    }
}

// ------------------------------------------------------------ running ----

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Panic,
    NonFinite,
    TooWide,
    Slow,
}

#[derive(Clone, Debug)]
struct Failure {
    kind: Kind,
    detail: String,
}

thread_local! {
    /// Where the last panic on this thread happened — filled by the quiet
    /// panic hook so a failure report can name the panicking line.
    static PANIC_AT: RefCell<Option<String>> = const { RefCell::new(None) };
    /// Set while a fuzz case runs: its panics are expected findings, caught
    /// and reported (shrink steps would otherwise print hundreds).
    static QUIET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Wrap the default panic hook: inside [`quietly`] only record the
/// location; everywhere else (a test's own assertion) print as usual.
fn install_quiet_panic_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let default_hook = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if QUIET.with(std::cell::Cell::get) {
                let at = info
                    .location()
                    .map(|l| format!("{}:{}", l.file(), l.line()))
                    .unwrap_or_default();
                PANIC_AT.with(|slot| *slot.borrow_mut() = Some(at));
            } else {
                default_hook(info);
            }
        }));
    });
}

/// `catch_unwind` with this thread's panics kept quiet.
fn quietly<R>(f: impl FnOnce() -> R) -> std::thread::Result<R> {
    install_quiet_panic_hook();
    QUIET.with(|q| q.set(true));
    let result = panic::catch_unwind(AssertUnwindSafe(f));
    QUIET.with(|q| q.set(false));
    result
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    let message = if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic>".to_string()
    };
    let at = PANIC_AT
        .with(|slot| slot.borrow_mut().take())
        .unwrap_or_default();
    format!("{message} at {at}")
}

/// The pipeline the 0.3 spec names, exactly as `BrowserState` runs it per
/// frame (minus the network).
fn run_pipeline(html_src: &str, css_src: &str) -> view::DocumentView {
    let document = html::parse(html_src).expect("html5ever never fails");
    let stylesheet = css::parse(css_src).expect("css::parse recovers");
    view::build_document_view(
        &document,
        &[stylesheet],
        800,
        None,
        &HashMap::new(),
        style::InteractionState::default(),
    )
    .expect("a parsed document always has a root")
}

const MAX_WIDTH: f32 = 1e7;

/// Judge one case on the current thread: panic, then layout invariants.
fn judge(case: &Case) -> Result<(), Failure> {
    let html_src = case.html();
    let css_src = case.css();
    let outcome = quietly(|| run_pipeline(&html_src, &css_src));
    let view = match outcome {
        Ok(view) => view,
        Err(payload) => {
            return Err(Failure {
                kind: Kind::Panic,
                detail: panic_message(payload.as_ref()),
            });
        }
    };
    let check_width = !case.has_unbounded_values();
    check_box(&view.layout_root, check_width, &mut Vec::new())?;
    check_commands(&view.commands)
}

fn box_tag(layout_box: &LayoutBox) -> String {
    match &layout_box.box_type {
        layout::BoxType::BlockNode(s)
        | layout::BoxType::FlexNode(s)
        | layout::BoxType::GridNode(s)
        | layout::BoxType::TableNode(s) => match &s.node_type {
            mb_dom::dom::NodeType::Element(el) => el.tag_name.clone(),
            mb_dom::dom::NodeType::Text(_) => "#text".to_string(),
        },
        layout::BoxType::AnonymousBlock => "#anon".to_string(),
    }
}

fn check_box(
    layout_box: &LayoutBox,
    check_width: bool,
    path: &mut Vec<String>,
) -> Result<(), Failure> {
    let d = &layout_box.dimensions;
    let numbers = [
        ("x", d.content.x),
        ("y", d.content.y),
        ("width", d.content.width),
        ("height", d.content.height),
        ("padding-left", d.padding.left),
        ("padding-right", d.padding.right),
        ("padding-top", d.padding.top),
        ("padding-bottom", d.padding.bottom),
        ("border-left", d.border.left),
        ("border-right", d.border.right),
        ("border-top", d.border.top),
        ("border-bottom", d.border.bottom),
        ("margin-left", d.margin.left),
        ("margin-right", d.margin.right),
        ("margin-top", d.margin.top),
        ("margin-bottom", d.margin.bottom),
    ];
    path.push(box_tag(layout_box));
    for (name, value) in numbers {
        if !value.is_finite() {
            return Err(Failure {
                kind: Kind::NonFinite,
                detail: format!("{name} = {value} on {}", path.join(" > ")),
            });
        }
    }
    let border_box =
        d.content.width + d.padding.left + d.padding.right + d.border.left + d.border.right;
    if check_width && border_box.abs() > MAX_WIDTH {
        return Err(Failure {
            kind: Kind::TooWide,
            detail: format!("border-box width {border_box} on {}", path.join(" > ")),
        });
    }
    for child in &layout_box.children {
        check_box(child, check_width, path)?;
    }
    path.pop();
    Ok(())
}

fn check_commands(commands: &[DisplayCommand]) -> Result<(), Failure> {
    for command in commands {
        let rect = match command {
            DisplayCommand::SolidRect(_, r) | DisplayCommand::RoundedRect(_, r, _) => Some(*r),
            DisplayCommand::Gradient(g) => Some(g.rect),
            DisplayCommand::BoxShadow(s) => Some(s.rect),
            DisplayCommand::Text(t) => Some(layout::Rect {
                x: t.x,
                y: t.y,
                width: t.font_size,
                height: t.wrap_width.unwrap_or(0.0),
            }),
            DisplayCommand::Image(i) => Some(layout::Rect {
                x: i.x,
                y: i.y,
                width: i.width,
                height: i.height,
            }),
            DisplayCommand::TransformGroup(_, inner) => {
                check_commands(inner)?;
                None
            }
        };
        if let Some(r) = rect
            && !(r.x.is_finite() && r.y.is_finite() && r.width.is_finite() && r.height.is_finite())
        {
            return Err(Failure {
                kind: Kind::NonFinite,
                detail: format!("paint command with non-finite geometry: {r:?}"),
            });
        }
    }
    Ok(())
}

/// Run one case on a fresh thread so a runaway case can be abandoned after
/// `cap` instead of hanging the test binary. The thread is leaked on a
/// timeout; the test then fails, which ends the process.
fn judge_with_cap(case: &Case, cap: Duration) -> (Result<(), Failure>, Duration) {
    let (tx, rx) = mpsc::channel();
    let owned = case.clone();
    let started = Instant::now();
    std::thread::Builder::new()
        .name("fuzz-case".into())
        .spawn(move || {
            let _ = tx.send(judge(&owned));
        })
        .expect("spawn fuzz thread");
    match rx.recv_timeout(cap) {
        Ok(result) => (result, started.elapsed()),
        Err(_) => (
            Err(Failure {
                kind: Kind::Slow,
                detail: format!("no result within {cap:?} (runaway?)"),
            }),
            started.elapsed(),
        ),
    }
}

/// Judge with the per-case caps of the mode: `hard` abandons the case,
/// `slow` (long mode only) fails a case that finished but took too long.
fn judge_timed(
    case: &Case,
    hard: Duration,
    slow: Option<Duration>,
) -> (Result<(), Failure>, Duration) {
    let (result, took) = judge_with_cap(case, hard);
    if result.is_ok()
        && let Some(limit) = slow
        && took > limit
    {
        return (
            Err(Failure {
                kind: Kind::Slow,
                detail: format!("took {took:?} (> {limit:?})"),
            }),
            took,
        );
    }
    (result, took)
}

// ----------------------------------------------------------- shrinking ----

/// Every case one step smaller than `case`: one node removed, one node
/// replaced by its children (unwrapping), one inline declaration, one rule
/// or one rule declaration removed. Order = biggest cuts first.
fn smaller_variants(case: &Case) -> Vec<Case> {
    let mut out = Vec::new();
    for i in 0..case.rules.len() {
        let mut c = case.clone();
        c.rules.remove(i);
        out.push(c);
    }
    let paths = node_paths(&case.body);
    for path in &paths {
        let mut c = case.clone();
        remove_at(&mut c.body, path);
        out.push(c);
    }
    for path in &paths {
        let mut c = case.clone();
        if unwrap_at(&mut c.body, path) {
            out.push(c);
        }
    }
    for (ri, rule) in case.rules.iter().enumerate() {
        for di in 0..rule.decls.len() {
            let mut c = case.clone();
            c.rules[ri].decls.remove(di);
            if c.rules[ri].decls.is_empty() {
                c.rules.remove(ri);
            }
            out.push(c);
        }
    }
    for path in &paths {
        let count = element_at(&case.body, path).map_or(0, |el| el.style.len() + el.attrs.len());
        for k in 0..count {
            let mut c = case.clone();
            if let Some(el) = element_at_mut(&mut c.body, path) {
                if k < el.style.len() {
                    el.style.remove(k);
                } else {
                    el.attrs.remove(k - el.style.len());
                }
            }
            out.push(c);
        }
    }
    for path in &paths {
        // Shorten text nodes (halve them).
        if let Some(Node::Text(t)) = node_at(&case.body, path)
            && t.chars().count() > 1
        {
            let mut c = case.clone();
            if let Some(Node::Text(t)) = node_at_mut(&mut c.body, path) {
                let half = t.chars().count() / 2;
                *t = t.chars().take(half).collect();
            }
            out.push(c);
        }
    }
    out
}

fn node_paths(nodes: &[Node]) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    fn walk(nodes: &[Node], prefix: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        for (i, node) in nodes.iter().enumerate() {
            prefix.push(i);
            out.push(prefix.clone());
            if let Node::Element(el) = node {
                walk(&el.children, prefix, out);
            }
            prefix.pop();
        }
    }
    walk(nodes, &mut Vec::new(), &mut out);
    out
}

fn node_at<'a>(nodes: &'a [Node], path: &[usize]) -> Option<&'a Node> {
    let (first, rest) = path.split_first()?;
    let node = nodes.get(*first)?;
    if rest.is_empty() {
        return Some(node);
    }
    match node {
        Node::Element(el) => node_at(&el.children, rest),
        Node::Text(_) => None,
    }
}

fn node_at_mut<'a>(nodes: &'a mut [Node], path: &[usize]) -> Option<&'a mut Node> {
    let (first, rest) = path.split_first()?;
    let node = nodes.get_mut(*first)?;
    if rest.is_empty() {
        return Some(node);
    }
    match node {
        Node::Element(el) => node_at_mut(&mut el.children, rest),
        Node::Text(_) => None,
    }
}

fn element_at<'a>(nodes: &'a [Node], path: &[usize]) -> Option<&'a Element> {
    match node_at(nodes, path)? {
        Node::Element(el) => Some(el),
        Node::Text(_) => None,
    }
}

fn element_at_mut<'a>(nodes: &'a mut [Node], path: &[usize]) -> Option<&'a mut Element> {
    match node_at_mut(nodes, path)? {
        Node::Element(el) => Some(el),
        Node::Text(_) => None,
    }
}

fn siblings_mut<'a>(nodes: &'a mut Vec<Node>, parent: &[usize]) -> Option<&'a mut Vec<Node>> {
    if parent.is_empty() {
        return Some(nodes);
    }
    match node_at_mut(nodes, parent)? {
        Node::Element(el) => Some(&mut el.children),
        Node::Text(_) => None,
    }
}

fn remove_at(nodes: &mut Vec<Node>, path: &[usize]) {
    let (last, parent) = path.split_last().expect("non-empty path");
    if let Some(list) = siblings_mut(nodes, parent)
        && *last < list.len()
    {
        list.remove(*last);
    }
}

/// Replace the element at `path` by its children. False for text / leaves.
fn unwrap_at(nodes: &mut Vec<Node>, path: &[usize]) -> bool {
    let (last, parent) = path.split_last().expect("non-empty path");
    let Some(list) = siblings_mut(nodes, parent) else {
        return false;
    };
    match list.get(*last) {
        Some(Node::Element(el)) if !el.children.is_empty() => {}
        _ => return false,
    }
    let Node::Element(el) = list.remove(*last) else {
        unreachable!("checked above");
    };
    for (offset, child) in el.children.into_iter().enumerate() {
        list.insert(*last + offset, child);
    }
    true
}

/// Greedy delta-debugging: take the first smaller variant that still fails
/// with the same kind, repeat until no variant does (or the step budget).
fn shrink(case: &Case, kind: &Kind, hard: Duration, slow: Option<Duration>) -> (Case, Failure) {
    let mut current = case.clone();
    let mut failure = judge_timed(&current, hard, slow)
        .0
        .expect_err("shrink starts from a failing case");
    let mut steps = 0;
    'outer: while steps < 20_000 {
        for candidate in smaller_variants(&current) {
            steps += 1;
            if let Err(f) = judge_timed(&candidate, hard, slow).0
                && f.kind == *kind
            {
                current = candidate;
                failure = f;
                continue 'outer;
            }
        }
        break;
    }
    (current, failure)
}

// ------------------------------------------------------------- drivers ----

struct Report {
    cases: u64,
    elapsed: Duration,
    slowest: (Duration, u64),
    unbounded_cases: u64,
    max_nodes: usize,
    /// First failure per (kind, detail prefix) signature.
    failures: BTreeMap<(Kind, String), (u64, Case, Failure)>,
}

fn count_nodes(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .map(|n| match n {
            Node::Text(_) => 1,
            Node::Element(el) => 1 + count_nodes(&el.children),
        })
        .sum()
}

fn signature(failure: &Failure) -> String {
    // Group panics by location, the rest by what failed (not by values).
    match failure.kind {
        Kind::Panic => failure
            .detail
            .rsplit(" at ")
            .next()
            .unwrap_or("")
            .to_string(),
        _ => failure
            .detail
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string(),
    }
}

fn run_seeds(
    first: u64,
    count: u64,
    hard: Duration,
    slow: Option<Duration>,
    keep_going: bool,
) -> Report {
    install_quiet_panic_hook();
    let started = Instant::now();
    let mut report = Report {
        cases: 0,
        elapsed: Duration::ZERO,
        slowest: (Duration::ZERO, first),
        unbounded_cases: 0,
        max_nodes: 0,
        failures: BTreeMap::new(),
    };
    for seed in first..first + count {
        let case = Gen::case(seed);
        report.cases += 1;
        report.max_nodes = report.max_nodes.max(count_nodes(&case.body));
        if case.has_unbounded_values() {
            report.unbounded_cases += 1;
        }
        let (result, took) = judge_timed(&case, hard, slow);
        if took > report.slowest.0 {
            report.slowest = (took, seed);
        }
        if let Err(failure) = result {
            let key = (failure.kind.clone(), signature(&failure));
            report.failures.entry(key).or_insert((seed, case, failure));
            if !keep_going || report.failures.len() >= 10 {
                break;
            }
        }
        if count >= 10_000 && report.cases.is_multiple_of(10_000) {
            eprintln!(
                "[fuzz] {} cases, {:.1}s, {} failure signature(s)",
                report.cases,
                started.elapsed().as_secs_f64(),
                report.failures.len()
            );
        }
    }
    report.elapsed = started.elapsed();
    report
}

/// Shrink and print every failure, then panic with a summary.
fn fail_with(report: &Report, hard: Duration, slow: Option<Duration>) {
    if report.failures.is_empty() {
        return;
    }
    let mut message = String::new();
    for ((kind, _), (seed, case, failure)) in &report.failures {
        let (small, small_failure) = shrink(case, kind, hard, slow);
        let _ = writeln!(
            message,
            "\n=== seed {seed}: {kind:?} — {}\n--- shrunk: {}\n--- shrunk html:\n{}\n--- shrunk css:\n{}",
            failure.detail,
            small_failure.detail,
            small.html(),
            small.css()
        );
    }
    eprintln!("{message}");
    panic!(
        "layout fuzzer found {} failure signature(s) in {} cases (details above)",
        report.failures.len(),
        report.cases
    );
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn maybe_install_fonts() {
    if std::env::var("MB_FUZZ_FONTS").is_ok_and(|v| v == "1") {
        mb_engine::font_system::install_fonts(&[]);
    }
}

#[test]
fn layout_fuzz_default_seeds_hold_the_invariants() {
    let hard = Duration::from_secs(5);
    let report = run_seeds(1, 200, hard, None, false);
    fail_with(&report, hard, None);
}

#[test]
#[ignore = "long run: MB_FUZZ_CASES=100000 cargo test --release -p mb-engine --test fuzz_layout -- --ignored --nocapture"]
fn layout_fuzz_long_run() {
    maybe_install_fonts();
    let first = env_u64("MB_FUZZ_SEED", 1);
    let count = env_u64("MB_FUZZ_CASES", 10_000);
    let hard = Duration::from_secs(5);
    let slow = Some(Duration::from_secs(1));
    let report = run_seeds(first, count, hard, slow, true);
    eprintln!(
        "[fuzz] layout: {} cases (seeds {first}..{}), {:.1}s total, {:.2} ms/case, slowest {:?} (seed {}), \
         unbounded-value cases {}, largest tree {} nodes, {} failure signature(s)",
        report.cases,
        first + report.cases,
        report.elapsed.as_secs_f64(),
        report.elapsed.as_secs_f64() * 1000.0 / report.cases.max(1) as f64,
        report.slowest.0,
        report.slowest.1,
        report.unbounded_cases,
        report.max_nodes,
        report.failures.len()
    );
    fail_with(&report, hard, slow);
}

// ------------------------------------------------- CSS token fuzzing ----

/// Token soup for the CSS parsers: unbalanced brackets, odd units, huge and
/// non-finite numbers, `!important` in every position, unterminated strings
/// and comments, escapes, non-ASCII.
const CSS_TOKENS: &[&str] = &[
    "{",
    "}",
    "(",
    ")",
    "[",
    "]",
    ";",
    ":",
    ",",
    "!",
    "!important",
    "! important",
    "!IMPORTANT",
    "/",
    "*",
    "+",
    ">",
    "~",
    "|",
    "#",
    ".",
    "&",
    "%",
    "@media",
    "@media screen and (min-width: 1px)",
    "@font-face",
    "@import",
    "@supports",
    "@layer",
    "@keyframes",
    "@",
    "url(",
    "url(\"a\")",
    "url(x",
    "\"",
    "'",
    "\"unterminated",
    "'é",
    "/*",
    "*/",
    "<!--",
    "-->",
    "\\",
    "\\66 oo",
    "\\0",
    "\\fffffff",
    "-",
    "--x",
    "--",
    "var(--x)",
    "var(",
    "calc(",
    "calc(1px + )",
    "min(",
    "max(1px,",
    "rgb(",
    "rgba(1,2,3,4,5)",
    "hsl(",
    "#fff",
    "#é1",
    "#한글",
    "#12345678",
    "#",
    "repeat(",
    "repeat(0,",
    "repeat(99999999, 1px)",
    "minmax(",
    "span",
    "fr",
    "1fr",
    "0",
    "-0",
    "1",
    "-1",
    "0.5",
    ".5e",
    "1e",
    "1e39",
    "-1e39",
    "1e-999",
    "3.4e38",
    "99999999999999999999",
    "NaN",
    "inf",
    "1px",
    "-1px",
    "1em",
    "1qq",
    "1\\px",
    "1%",
    "1e39%",
    "100vw",
    "1dppx",
    "+-1",
    "1.2.3",
    "a",
    "div",
    "*",
    "*|*",
    "|a",
    "a|",
    "::before",
    ":hover",
    ":not(",
    ":not(a)",
    ":nth-child(2n+1)",
    ":nth-child(",
    ":is(",
    ":has(",
    "[a=",
    "[a=\"b\"]",
    "[a~",
    "a > > b",
    "display",
    "width",
    "margin",
    "padding",
    "flex",
    "grid-template-columns",
    "grid-template-areas",
    "grid-row",
    "background",
    "border-radius",
    "box-shadow",
    "transform",
    "rotate(",
    "matrix(1,2,3,4,5,6)",
    "translate(1e39px)",
    "scale(",
    "linear-gradient(",
    "radial-gradient(red,",
    "inset",
    "solid",
    "none",
    "auto",
    "inherit",
    "\"a b\" \"c\"",
    "\"\"",
    "\".\"",
    "안녕",
    "😀",
    "\u{0}",
    "\u{feff}",
    "\u{fffd}",
    " ",
    "  ",
    "\n",
    "\t",
];

/// Property names whose values go through a dedicated parser — the token
/// soup is aimed at them half the time.
const CSS_DEDICATED: &[&str] = &[
    "background",
    "background-position",
    "border-radius",
    "box-shadow",
    "text-shadow",
    "transform",
    "flex",
    "margin",
    "padding",
    "grid-template-columns",
    "grid-template-rows",
    "grid-template-areas",
    "grid-column",
    "grid-row",
    "font-size",
    "width",
    "color",
    "background-color",
    "line-height",
    "border",
    "font",
    "gap",
];

fn token_soup(rng: &mut Rng, len: usize) -> String {
    let mut out = String::new();
    for _ in 0..len {
        out.push_str(rng.pick_str(CSS_TOKENS));
        if rng.chance(50) {
            out.push(' ');
        }
    }
    out
}

/// A CSS input: pure soup, or rule-shaped text with soup values (so the
/// dedicated value parsers actually see the garbage). Returns the
/// stylesheet text and, separately, the declaration bodies (what a
/// `style=""` attribute would hold).
fn css_input(rng: &mut Rng) -> (String, String) {
    if rng.chance(30) {
        let len = rng.range(0, 60);
        let soup = token_soup(rng, len);
        return (soup.clone(), soup);
    }
    const SELECTORS: &[&str] = &[
        "*", "div", ".a", "p span", "div > a", "td", "ul li", "input", "button",
    ];
    let mut sheet = String::new();
    let mut bodies = String::new();
    for _ in 0..rng.range(1, 5) {
        if rng.chance(25) {
            let selector_len = rng.range(1, 4);
            sheet.push_str(&token_soup(rng, selector_len));
        } else {
            sheet.push_str(rng.pick_str(SELECTORS));
        }
        sheet.push('{');
        let mut body = String::new();
        for _ in 0..rng.range(1, 5) {
            let property = *rng.pick(CSS_DEDICATED);
            body.push_str(property);
            body.push(':');
            body.push_str(&soup_value(rng, property));
            if rng.chance(85) {
                body.push(';');
            }
        }
        sheet.push_str(&body);
        bodies.push_str(&body);
        if rng.chance(90) {
            sheet.push('}');
        }
    }
    (sheet, bodies)
}

/// Mostly a near-valid value for `property` with its numbers swapped for
/// odd ones (huge, negative, non-finite, strange units) and a stray token
/// sometimes spliced in; otherwise plain soup.
fn soup_value(rng: &mut Rng, property: &str) -> String {
    if rng.chance(30) {
        let len = rng.range(0, 8);
        return token_soup(rng, len);
    }
    let template = match property {
        "background" => *rng.pick(&[
            "red url(a.png) 1px 2px no-repeat",
            "linear-gradient(90deg, red 1%, blue)",
            "#fff",
        ]),
        "background-position" => *rng.pick(&["1px 2px", "left 1px top", "center 50%"]),
        "border-radius" => *rng.pick(&["1px", "1px 2px 3px 4px / 5px", "50%"]),
        "box-shadow" | "text-shadow" => {
            *rng.pick(&["1px 2px 3px red", "inset 0 0 1px 2px #000", "1px 1px"])
        }
        "transform" => *rng.pick(&[
            "rotate(1deg) translate(1px, 2px)",
            "scale(2) matrix(1,0,0,1,1,1)",
            "translateX(1%)",
        ]),
        "flex" => *rng.pick(&["1 1 0%", "2 3", "1px", "none"]),
        "grid-template-columns" | "grid-template-rows" => *rng.pick(&[
            "1fr 2fr",
            "repeat(3, 1px) minmax(1px, 1fr)",
            "1px auto 1%",
            "repeat(2, 1fr 1px)",
        ]),
        "grid-template-areas" => *rng.pick(&["\"a b\" \"c d\"", "\"a\" \"a b\"", "\". .\""]),
        "grid-column" | "grid-row" => *rng.pick(&["1 / span 2", "-1 / 1", "a / b", "span 1"]),
        "font" => *rng.pick(&["bold 1px/2 serif", "1px a", "italic 1em b"]),
        "border" => *rng.pick(&["1px solid red", "1px", "thick dashed"]),
        _ => *rng.pick(&["1px", "1px 2px 3px 4px", "1%", "1em", "1", "red"]),
    };
    const NUMBERS: &[&str] = &[
        "0",
        "-1",
        "1e39",
        "-1e39",
        "3e38",
        "1e-39",
        "99999999999",
        "0.0000001",
        "1e9",
        "-0",
        "2147483648",
    ];
    const UNITS: &[&str] = &["px", "%", "em", "fr", "deg", "qq", "", "e", "vw", "rem"];
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '1' && rng.chance(50) {
            out.push_str(rng.pick_str(NUMBERS));
            // Swallow the template's own unit sometimes and put a strange one.
            if rng.chance(30) {
                while chars
                    .peek()
                    .is_some_and(|c| c.is_ascii_alphabetic() || *c == '%')
                {
                    chars.next();
                }
                out.push_str(rng.pick_str(UNITS));
            }
        } else {
            out.push(ch);
        }
        if ch == ' ' && rng.chance(5) {
            out.push_str(rng.pick_str(CSS_TOKENS));
            out.push(' ');
        }
    }
    if rng.chance(10) {
        out.push_str(" !important");
    }
    out
}

/// A small fixed page whose elements every soup stylesheet can match, so
/// whatever survives parsing also goes through cascade + layout + paint.
const SOUP_PAGE: &str = "<!DOCTYPE html><html><body><div class=\"a\"><p>text 안녕</p><span>x</span>\
     <div style=\"display:grid\"><a href=\"#\">l</a><div>g</div></div><table><tr><td>c</td></tr></table>\
     <ul><li>i</li></ul><input type=\"text\"><button>b</button><img alt=\"i\"></div></body></html>";

/// One soup input through every parser entry point, then (if anything
/// survived) cascade + layout + paint + rasterization on [`SOUP_PAGE`]. Ok = how many
/// declarations survived parsing, so the long run can show the soup
/// actually reaches the value parsers.
fn css_soup_case(seed: u64) -> Result<usize, Failure> {
    let mut rng = Rng::new(seed ^ 0xC55_F022);
    let (source, bodies) = css_input(&mut rng);
    let short_len = rng.range(0, 12);
    let outcome = quietly(|| {
        let sheet = css::parse(&source).expect("css::parse recovers");
        let _ = css::parse_with_report(&source);
        let inline = css::parse_inline_style(&bodies);
        let _ = css::parse_inline_style(&source);
        let _ = css::parse_selector(&source);
        // Short prefixes too: `parse_selector` mostly sees short inputs.
        let short: String = source.chars().take(short_len).collect();
        let _ = css::parse_selector(&short);
        let survived = sheet
            .rules
            .iter()
            .map(|r| r.declarations.len())
            .sum::<usize>()
            + inline.len();
        if !sheet.rules.is_empty() {
            let document = html::parse(SOUP_PAGE).expect("html5ever never fails");
            let view = view::build_document_view(
                &document,
                &[sheet],
                800,
                None,
                &HashMap::new(),
                style::InteractionState::default(),
            )
            .expect("a parsed document always has a root");
            // And the pixels: soup transforms / shadows / radii reach
            // tiny-skia the way the window and agent screenshots do.
            let _ = mb_engine::render::rasterize(&view.commands, 400, 300);
        }
        survived
    });
    outcome.map_err(|payload| Failure {
        kind: Kind::Panic,
        detail: format!("{} — css: {source:?}", panic_message(payload.as_ref())),
    })
}

struct SoupReport {
    failures: Vec<(u64, Failure)>,
    survived_declarations: usize,
}

fn run_css_soup(first: u64, count: u64) -> SoupReport {
    install_quiet_panic_hook();
    let mut report = SoupReport {
        failures: Vec::new(),
        survived_declarations: 0,
    };
    let mut seen = std::collections::BTreeSet::new();
    for seed in first..first + count {
        match css_soup_case(seed) {
            Ok(n) => report.survived_declarations += n,
            Err(failure) => {
                if seen.insert(signature(&failure)) {
                    report.failures.push((seed, failure));
                    if report.failures.len() >= 10 {
                        break;
                    }
                }
            }
        }
    }
    report
}

fn fail_css(failures: &[(u64, Failure)], count: u64) {
    if failures.is_empty() {
        return;
    }
    for (seed, failure) in failures {
        eprintln!("=== css seed {seed}: {}", failure.detail);
    }
    panic!(
        "css token fuzzer: {} panic site(s) in {count} inputs",
        failures.len()
    );
}

#[test]
fn css_token_fuzz_never_panics() {
    let report = run_css_soup(1, 500);
    fail_css(&report.failures, 500);
    // The soup must reach the value parsers, not just bounce off selectors.
    assert!(
        report.survived_declarations > 100,
        "only {} declarations survived 500 inputs",
        report.survived_declarations
    );
}

#[test]
#[ignore = "long run: MB_FUZZ_CASES=100000 cargo test --release -p mb-engine --test fuzz_layout -- --ignored --nocapture"]
fn css_token_fuzz_long_run() {
    let first = env_u64("MB_FUZZ_SEED", 1);
    let count = env_u64("MB_FUZZ_CASES", 10_000);
    let started = Instant::now();
    let report = run_css_soup(first, count);
    eprintln!(
        "[fuzz] css: {count} inputs, {:.1}s, {} declarations survived parsing, {} panic site(s)",
        started.elapsed().as_secs_f64(),
        report.survived_declarations,
        report.failures.len()
    );
    fail_css(&report.failures, count);
}

// --------------------------------------------- D2: image byte fuzzing ----

/// docs/compat-round3.md D2: image bytes are untrusted input too. Cheap and
/// structured rather than coverage-guided: the risk worth hunting is a
/// header that *claims* a size (an SVG `width="1e9"`, a PNG IHDR of
/// 2^31 × 2^31) and gets an allocation — an abort, not a panic — or a
/// decode that takes seconds. The raster decoders themselves are the
/// `image` crate's (fuzzed upstream); this checks our glue and its limits.
fn image_case(seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed ^ 0x1_3A6E);
    let is_svg = rng.below(6) < 2;
    let mut bytes = match rng.below(6) {
        _ if is_svg => svg_bytes(&mut rng),
        2 => png_bytes(&mut rng),
        3 => {
            let mut b = b"GIF89a".to_vec();
            b.extend(odd_u16(&mut rng).to_le_bytes());
            b.extend(odd_u16(&mut rng).to_le_bytes());
            b.push(*rng.pick(&[0x00, 0x80, 0xF7, 0xFF]));
            b
        }
        4 => {
            let mut b = b"BM".to_vec();
            b.extend([0u8; 12]);
            b.extend(40u32.to_le_bytes());
            b.extend((odd_u32(&mut rng) as i32).to_le_bytes());
            b.extend((odd_u32(&mut rng) as i32).to_le_bytes());
            b.extend(1u16.to_le_bytes());
            b.extend((*rng.pick(&[1u16, 4, 8, 16, 24, 32, 0, 65535])).to_le_bytes());
            b
        }
        _ => {
            let mut b = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08];
            b.extend(odd_u16(&mut rng).to_be_bytes());
            b.extend(odd_u16(&mut rng).to_be_bytes());
            b.push(3);
            b
        }
    };
    // Noise: random tail, truncation, or a few flipped bytes. Rarer for
    // SVG, where any noise mostly just makes the XML invalid.
    let noise = if is_svg && rng.chance(75) {
        3
    } else {
        rng.below(4)
    };
    match noise {
        0 => {
            for _ in 0..rng.range(0, 256) {
                bytes.push(rng.next() as u8);
            }
        }
        1 => {
            let keep = rng.range(0, bytes.len());
            bytes.truncate(keep);
        }
        2 if !bytes.is_empty() => {
            for _ in 0..rng.range(1, 4) {
                let i = rng.below(bytes.len());
                bytes[i] ^= 1 << rng.below(8);
            }
        }
        _ => {}
    }
    bytes
}

fn odd_u16(rng: &mut Rng) -> u16 {
    *rng.pick(&[0, 1, 2, 255, 4096, 32767, 65535])
}

fn odd_u32(rng: &mut Rng) -> u32 {
    *rng.pick(&[
        0,
        1,
        3,
        4096,
        65535,
        1 << 20,
        0x7FFF_FFFF,
        0x8000_0000,
        u32::MAX,
    ])
}

fn svg_bytes(rng: &mut Rng) -> Vec<u8> {
    const SIZES: &[&str] = &[
        "1", "16.5", "4097", "50000", "1e9", "3e38", "1e39", "1", "16", "300", "0", "-1", "100%",
        "NaN", "abc",
    ];
    const BODIES: &[&str] = &[
        "",
        "<rect width=\"100%\" height=\"100%\" fill=\"red\"/>",
        "<circle r=\"1e30\" stroke-width=\"1e30\" stroke=\"blue\"/>",
        "<g transform=\"scale(1e30) rotate(1e9)\"><rect width=\"1\" height=\"1\"/></g>",
        "<path d=\"M0 0 L1e39 1e39 Z M NaN 1 C\"/>",
        "<filter id=\"f\"><feGaussianBlur stdDeviation=\"1e6\"/></filter><rect width=\"9\" height=\"9\" filter=\"url(#f)\"/>",
        "<use href=\"#u\" id=\"u\"/>",
        "<text font-size=\"1e9\">안녕</text>",
        "<image width=\"1e9\" height=\"1e9\" href=\"data:image/png;base64,AAAA\"/>",
    ];
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\">{}</svg>",
        rng.pick(SIZES),
        rng.pick(SIZES),
        rng.pick(SIZES),
        rng.pick(SIZES),
        rng.pick(BODIES)
    )
    .into_bytes()
}

/// A PNG signature + IHDR with odd dimensions and a *correct* CRC (so the
/// decoder gets as far as the size check) + a junk IDAT.
fn png_bytes(rng: &mut Rng) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = b"IHDR".to_vec();
    ihdr.extend(odd_u32(rng).to_be_bytes());
    ihdr.extend(odd_u32(rng).to_be_bytes());
    ihdr.push(*rng.pick(&[1, 8, 16]));
    ihdr.push(*rng.pick(&[0, 2, 3, 4, 6]));
    ihdr.extend([0, 0, *rng.pick(&[0, 1])]);
    push_chunk(&mut b, &ihdr);
    let mut idat = b"IDAT".to_vec();
    idat.extend([0x78, 0x9C]);
    for _ in 0..rng.range(0, 64) {
        idat.push(rng.next() as u8);
    }
    push_chunk(&mut b, &idat);
    push_chunk(&mut b, b"IEND");
    b
}

fn push_chunk(out: &mut Vec<u8>, type_and_data: &[u8]) {
    out.extend(((type_and_data.len() - 4) as u32).to_be_bytes());
    out.extend(type_and_data);
    out.extend(crc32(type_and_data).to_be_bytes());
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// One image case on a capped thread: panic, time, and the decoded
/// buffer's consistency.
/// Ok(Some(size)) = decoded, Ok(None) = rejected cleanly.
type ImageVerdict = Result<Option<(usize, usize)>, Failure>;

fn judge_image(seed: u64, cap: Duration) -> (ImageVerdict, Duration) {
    let bytes = image_case(seed);
    // An abort (allocation failure, a panic that can't unwind) kills the
    // process before any report: `MB_FUZZ_DUMP=1` prints each input first.
    if std::env::var_os("MB_FUZZ_DUMP").is_some() {
        eprintln!(
            "[fuzz] image seed {seed}: {:?}",
            String::from_utf8_lossy(&bytes)
        );
    }
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    std::thread::spawn(move || {
        let url = mb_dom::url::Url::parse("http://fuzz.test/i").expect("static url");
        let outcome = quietly(|| mb_dom::resource::decode_image(url, &bytes));
        let verdict = match outcome {
            Err(payload) => Err(Failure {
                kind: Kind::Panic,
                detail: format!(
                    "{} — bytes {:?}",
                    panic_message(payload.as_ref()),
                    String::from_utf8_lossy(&bytes)
                ),
            }),
            Ok(Ok(image)) if image.pixels.len() != image.width * image.height => Err(Failure {
                kind: Kind::NonFinite,
                detail: format!(
                    "{}×{} image with {} pixels",
                    image.width,
                    image.height,
                    image.pixels.len()
                ),
            }),
            Ok(Ok(image)) => Ok(Some((image.width, image.height))),
            Ok(Err(_)) => Ok(None),
        };
        let _ = tx.send(verdict);
    });
    match rx.recv_timeout(cap) {
        Ok(verdict) => (verdict, started.elapsed()),
        Err(_) => (
            Err(Failure {
                kind: Kind::Slow,
                detail: format!("image seed {seed}: no result within {cap:?}"),
            }),
            started.elapsed(),
        ),
    }
}

struct ImageReport {
    failures: Vec<(u64, Failure)>,
    decoded: usize,
    largest: (usize, usize),
    slowest: Duration,
}

fn run_images(first: u64, count: u64, hard: Duration, slow: Option<Duration>) -> ImageReport {
    install_quiet_panic_hook();
    let mut report = ImageReport {
        failures: Vec::new(),
        decoded: 0,
        largest: (0, 0),
        slowest: Duration::ZERO,
    };
    for seed in first..first + count {
        let (mut verdict, took) = judge_image(seed, hard);
        report.slowest = report.slowest.max(took);
        if verdict.is_ok()
            && let Some(limit) = slow
            && took > limit
        {
            verdict = Err(Failure {
                kind: Kind::Slow,
                detail: format!("image seed {seed} took {took:?}"),
            });
        }
        match verdict {
            Ok(Some((w, h))) => {
                report.decoded += 1;
                if w * h > report.largest.0 * report.largest.1 {
                    report.largest = (w, h);
                }
            }
            Ok(None) => {}
            Err(failure) => {
                report.failures.push((seed, failure));
                if report.failures.len() >= 10 {
                    break;
                }
            }
        }
    }
    report
}

#[test]
fn image_bytes_fuzz_never_panics_or_balloons() {
    let report = run_images(1, 300, Duration::from_secs(5), None);
    fail_css(&report.failures, 300);
    assert!(
        report.decoded >= 30,
        "only {} of 300 images decoded",
        report.decoded
    );
}

#[test]
#[ignore = "long run: MB_FUZZ_CASES=100000 cargo test --release -p mb-engine --test fuzz_layout -- --ignored --nocapture"]
fn image_bytes_fuzz_long_run() {
    let first = env_u64("MB_FUZZ_SEED", 1);
    let count = env_u64("MB_FUZZ_CASES", 10_000);
    let started = Instant::now();
    let report = run_images(
        first,
        count,
        Duration::from_secs(5),
        Some(Duration::from_secs(1)),
    );
    eprintln!(
        "[fuzz] images: {count} inputs, {:.1}s, {} decoded (largest {}×{}), slowest {:?}, {} failure(s)",
        started.elapsed().as_secs_f64(),
        report.decoded,
        report.largest.0,
        report.largest.1,
        report.slowest,
        report.failures.len()
    );
    fail_css(&report.failures, count);
}

// ------------------------------------------------ D1: deep nesting ----

const DEEP_CHILD_ENV: &str = "MB_FUZZ_DEEP_CHILD";

/// docs/compat-round3.md D1: the structured generator stops at depth 40, so
/// one fixed case nests 10 000 levels (mixed block / inline / flex / grid
/// wrappers). Recursion that deep is a stack overflow — an **abort**, which
/// `catch_unwind` can't turn into a test failure — so the case runs in a
/// child process (this same test binary, filtered to this test) and the
/// parent checks its exit status. The child uses an 8 MB thread: the stack
/// the browser actually runs on (the main thread of mb-mcp / mb-shell).
#[test]
fn ten_thousand_levels_of_nesting_do_not_overflow_the_stack() {
    if std::env::var_os(DEEP_CHILD_ENV).is_some() {
        run_deep_case();
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let output = std::process::Command::new(exe)
        .args([
            "ten_thousand_levels_of_nesting_do_not_overflow_the_stack",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(DEEP_CHILD_ENV, "1")
        .output()
        .expect("run the child test process");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("[deep] ok"),
        "deep nesting child failed ({:?}):\n{stderr}\n{stdout}",
        output.status
    );
}

fn run_deep_case() {
    const DEPTH: usize = 10_000;
    // Block-level only: an inline wrapper re-sums its whole subtree and so
    // does every inline below it — O(n × inline depth), 6.9 s in release
    // for 10 000 nested `<span>`s (docs/compat-round3.md D1 notes). That's
    // a time problem, tracked separately; this test is about the stack.
    const WRAPPERS: &[&str] = &[
        "<div>",
        "<div class=\"f\">",
        "<ul>",
        "<div class=\"g\">",
        "<form>",
        "<div>",
        "<section>",
    ];
    let mut html_src = String::from("<!DOCTYPE html><html><body>");
    let mut closers = Vec::with_capacity(DEPTH);
    for i in 0..DEPTH {
        let open = WRAPPERS[i % WRAPPERS.len()];
        html_src.push_str(open);
        if i % 97 == 0 {
            html_src.push_str("text 안녕 ");
        }
        let tag = open
            .trim_start_matches('<')
            .split([' ', '>'])
            .next()
            .unwrap_or("div");
        closers.push(format!("</{tag}>"));
    }
    html_src.push_str("<a href=\"#x\">deepest</a>");
    for closer in closers.iter().rev() {
        html_src.push_str(closer);
    }
    html_src.push_str("</body></html>");
    let css_src = ".f { display: flex; padding: 1px } .g { display: grid; grid-template-columns: 1fr 1fr } ul { margin-left: 1px }";
    let worker = std::thread::Builder::new()
        .name("deep-case".into())
        .stack_size(8 << 20)
        .spawn(move || {
            let view = run_pipeline(&html_src, css_src);
            check_box(&view.layout_root, true, &mut Vec::new()).map_err(|f| f.detail)?;
            check_commands(&view.commands).map_err(|f| f.detail)?;
            Ok::<usize, String>(view.commands.len())
        })
        .expect("spawn deep-case thread");
    match worker.join() {
        Ok(Ok(commands)) => println!("[deep] ok ({commands} paint commands)"),
        Ok(Err(detail)) => panic!("deep case broke a layout invariant: {detail}"),
        Err(_) => panic!("deep case panicked"),
    }
}

// ------------------------------------------------- generator sanity ----

/// The generator must actually reach its advertised shape — deep trees,
/// both value pools, every verdict path — or a green run means nothing.
#[test]
fn fuzz_generator_covers_its_spec() {
    let mut deepest = 0;
    let mut unbounded = 0;
    let mut with_areas = 0;
    let mut korean = 0;
    for seed in 1..=200 {
        let case = Gen::case(seed);
        deepest = deepest.max(tree_depth(&case.body));
        if case.has_unbounded_values() {
            unbounded += 1;
        }
        if case.css().contains("grid-template-areas") {
            with_areas += 1;
        }
        if case.html().contains('한') || case.html().contains('안') || case.html().contains('가')
        {
            korean += 1;
        }
    }
    assert!(
        deepest >= 30,
        "deepest generated tree is only {deepest} levels"
    );
    assert!(
        (20..=80).contains(&unbounded),
        "{unbounded}/200 unbounded cases"
    );
    assert!(
        with_areas >= 10,
        "{with_areas}/200 cases with grid-template-areas"
    );
    assert!(korean >= 20, "{korean}/200 cases with Korean text");
    // Determinism: the same seed builds the same case.
    assert_eq!(Gen::case(42).html(), Gen::case(42).html());
    assert_eq!(Gen::case(42).css(), Gen::case(42).css());
}

fn tree_depth(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .map(|n| match n {
            Node::Text(_) => 1,
            Node::Element(el) => 1 + tree_depth(&el.children),
        })
        .max()
        .unwrap_or(0)
}

/// The shrinker itself: a synthetic predicate ("contains a `td` with
/// `colspan`") must shrink a big case down to just that element.
#[test]
fn shrinker_reduces_to_the_failing_part() {
    let case = (1..500)
        .map(Gen::case)
        .find(|c| c.html().contains("colspan") && count_nodes(&c.body) > 20)
        .expect("some seed generates a colspan cell in a big tree");
    let fails = |c: &Case| c.html().contains("colspan");
    let mut current = case.clone();
    'outer: loop {
        for candidate in smaller_variants(&current) {
            if fails(&candidate) {
                current = candidate;
                continue 'outer;
            }
        }
        break;
    }
    assert!(current.rules.is_empty());
    assert_eq!(count_nodes(&current.body), 1, "{}", current.html());
    assert!(
        current.html().contains("<td colspan="),
        "{}",
        current.html()
    );
}
