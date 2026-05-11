// User-agent default styles — the tiny stylesheet that makes raw HTML
// without any author CSS still legible. Every tag the toy renders gets
// at most a few key declarations here (display mode, margins, font
// hints, link color); everything else falls through to the inherited /
// initial cascade. Author rules and `style=` attributes win because
// the cascade applies UA defaults before matched declarations.

use crate::{
    css::Value,
    dom::{Document, NodeId, NodeType},
};

use super::PropertyMap;

pub(super) fn default_values(document: &Document, node_id: NodeId) -> PropertyMap {
    let mut values = PropertyMap::new();
    let element = match document.get(node_id).map(|n| &n.node_type) {
        Some(NodeType::Element(element)) => element,
        _ => return values,
    };

    // These defaults act like a tiny user-agent stylesheet so unstyled pages remain legible.
    match element.tag_name.as_str() {
        // HTML5 default: these are non-rendered "metadata" / scripting elements.
        // Without this, a `<script>` body shows up as raw text in the page (the
        // single biggest visual noise on naver-style sites). The full set
        // matches the spec category for "metadata content + scripting".
        "head" | "title" | "meta" | "link" | "script" | "style" | "noscript" => {
            values.insert("display".into(), Value::Keyword("none".into()));
        }
        "body" => {
            edge_defaults(&mut values, "margin", 8.0);
        }
        // Legacy `<center>`: text-align centers inline descendants
        // (text, inline images, inline-block buttons). Centering of
        // block children — the historical reason this tag still
        // appears on real pages — is handled in `specified_values`
        // by injecting `margin-left/right: auto` on those children.
        "center" => {
            values.insert("text-align".into(), Value::Keyword("center".into()));
        }
        "p" => {
            values.insert(
                "margin-top".into(),
                Value::Length(12.0, crate::css::Unit::Px),
            );
            values.insert(
                "margin-bottom".into(),
                Value::Length(12.0, crate::css::Unit::Px),
            );
        }
        "h1" => {
            values.insert(
                "font-size".into(),
                Value::Length(32.0, crate::css::Unit::Px),
            );
            values.insert(
                "margin-top".into(),
                Value::Length(12.0, crate::css::Unit::Px),
            );
            values.insert(
                "margin-bottom".into(),
                Value::Length(16.0, crate::css::Unit::Px),
            );
            values.insert("font-weight".into(), Value::Keyword("bold".into()));
        }
        // h2..h6 get the bold weight every browser ships in its UA stylesheet
        // even though we don't size them yet — without it, headings look
        // visually identical to body copy on pages that rely on the UA
        // default for emphasis. Sizes are a follow-up; bolding alone is
        // enough to restore reading hierarchy on plain HTML.
        "h2" | "h3" | "h4" | "h5" | "h6" => {
            values.insert("font-weight".into(), Value::Keyword("bold".into()));
        }
        // The HTML emphasis tags. Author CSS still wins because UA defaults
        // run before matched declarations.
        "b" | "strong" => {
            values.insert("font-weight".into(), Value::Keyword("bold".into()));
        }
        // <hr> renders as a 1px gray rule that spans its containing
        // block. UA stylesheets land it via a top border on a zero-height
        // box (the historical Netscape rendering), with vertical margins
        // so adjacent paragraphs don't kiss the line. The border-color
        // fallback in render::border_commands picks `currentColor` when
        // no explicit color is set, which lands at the dark text color
        // here for typical pages.
        "hr" => {
            values.insert("border-top".into(), Value::Length(1.0, crate::css::Unit::Px));
            values.insert(
                "border-color".into(),
                Value::Color(crate::css::Color {
                    r: 204,
                    g: 204,
                    b: 204,
                    a: 255,
                }),
            );
            values.insert(
                "margin-top".into(),
                Value::Length(8.0, crate::css::Unit::Px),
            );
            values.insert(
                "margin-bottom".into(),
                Value::Length(8.0, crate::css::Unit::Px),
            );
            // Zero content height: the visible line *is* the top border;
            // a non-zero content area would push following blocks down by
            // an extra row of empty pixels.
            values.insert("height".into(), Value::Length(0.0, crate::css::Unit::Px));
        }
        // <sup>/<sub> get the UA defaults that make them read as
        // typographic super/subscript: smaller glyph + raised/lowered
        // baseline. Without these the footnote markers on the Haskell
        // blog (and the bug-report `<sup>` markers on legacy GitHub
        // issues) render as full-size digits squatting at the top of the
        // line — visibly broken even before the line-height issue gets
        // factored in.
        "sup" => {
            values.insert("font-size".into(), Value::Keyword("smaller".into()));
            values.insert("vertical-align".into(), Value::Keyword("super".into()));
        }
        "sub" => {
            values.insert("font-size".into(), Value::Keyword("smaller".into()));
            values.insert("vertical-align".into(), Value::Keyword("sub".into()));
        }
        "a" => {
            values.insert(
                "color".into(),
                Value::Color(crate::css::Color {
                    r: 0,
                    g: 102,
                    b: 204,
                    a: 255,
                }),
            );
            // The visual underline already lives in display_list (a link with
            // an href emits an underline command unless `text-decoration: none`
            // is in scope). Surfacing it here as a UA default makes the cascade
            // spec-correct: author CSS or runtime style queries see the same
            // value the renderer is acting on.
            values.insert("text-decoration".into(), Value::Keyword("underline".into()));
        }
        // <input> and <textarea> both render as atomic inline-block
        // widgets. The UA stylesheet gives them a fixed default width
        // (so an unstyled field still has a usable click target), a
        // 1px gray border + white background so the box silhouette
        // reads as a text field, and small horizontal padding so the
        // caret + value text don't kiss the border. <textarea> uses
        // the same shell — its multi-line behaviour is purely about
        // how `intrinsic_height` and the value-text commands handle
        // the value buffer. Author CSS still wins because UA defaults
        // are applied before matched declarations.
        // Table-family tags get the display values that flip them onto the
        // dedicated table layout path. Without these the parser still produces
        // the right tree shape but every <td> would render as a block, so
        // tabular content collapses into a single column. Author CSS still
        // wins because UA defaults run before matched declarations — pages
        // that explicitly do `table { display: block }` (mobile reflow trick)
        // still get the override they expect.
        //
        // thead/tbody/tfoot map to `table-row-group`; the table layout walker
        // treats those groups as transparent and harvests their <tr> children
        // directly. caption / col / colgroup are not yet handled by the
        // layout walker, so they fall through to default block rendering.
        "table" => {
            values.insert("display".into(), Value::Keyword("table".into()));
            // Default border-spacing matches HTML's traditional 2px gap
            // between cells. presentational_hints already overrides this
            // when `cellspacing` is on the tag — and author CSS overrides
            // both.
            values.insert(
                "border-spacing".into(),
                Value::Length(2.0, crate::css::Unit::Px),
            );
            // CSS spec says text-align inherits, but real browsers reset
            // it at the table boundary so an outer `<center>` (or any
            // ancestor with `text-align: center`) does not centre every
            // cell's content. Without this reset, HN inside `<center>`
            // ends up with all cell text centred — visually wrong even
            // though the cell columns themselves align correctly.
            values.insert("text-align".into(), Value::Keyword("left".into()));
        }
        "thead" | "tbody" | "tfoot" => {
            values.insert(
                "display".into(),
                Value::Keyword("table-row-group".into()),
            );
        }
        "tr" => {
            values.insert("display".into(), Value::Keyword("table-row".into()));
        }
        "td" => {
            values.insert("display".into(), Value::Keyword("table-cell".into()));
            // No UA padding default for now: the toy CSS parser doesn't expand
            // the `padding` shorthand, so a non-zero default here would be
            // permanently locked in for any page that resets cell padding via
            // `td { padding: 0 }`. Real browsers default to ~1px; once
            // shorthand expansion lands we can restore that.
        }
        "th" => {
            // <th> is the header-cell variant of <td>: same table-cell display,
            // but UA stylesheets add bold weight + centered text so a header
            // row stands out without author CSS. Author rules still win.
            values.insert("display".into(), Value::Keyword("table-cell".into()));
            values.insert("font-weight".into(), Value::Keyword("bold".into()));
            values.insert("text-align".into(), Value::Keyword("center".into()));
        }
        // Real browsers give `<pre>` `white-space: pre`, a monospace font,
        // and a small vertical margin. Without these UA defaults a code
        // block on an unstyled page collapses its newlines (looks like one
        // long line) and renders in the proportional fallback (visually
        // indistinguishable from prose). Padding + a faint background let
        // the block read as a code panel even before any author CSS lands;
        // author rules win because UA defaults are applied before matched
        // declarations.
        "pre" => {
            values.insert("white-space".into(), Value::Keyword("pre".into()));
            values.insert("font-family".into(), Value::Keyword("monospace".into()));
            values.insert(
                "margin-top".into(),
                Value::Length(12.0, crate::css::Unit::Px),
            );
            values.insert(
                "margin-bottom".into(),
                Value::Length(12.0, crate::css::Unit::Px),
            );
            values.insert(
                "padding-top".into(),
                Value::Length(8.0, crate::css::Unit::Px),
            );
            values.insert(
                "padding-bottom".into(),
                Value::Length(8.0, crate::css::Unit::Px),
            );
            values.insert(
                "padding-left".into(),
                Value::Length(10.0, crate::css::Unit::Px),
            );
            values.insert(
                "padding-right".into(),
                Value::Length(10.0, crate::css::Unit::Px),
            );
            values.insert(
                "background-color".into(),
                Value::Color(crate::css::Color {
                    r: 246,
                    g: 248,
                    b: 250,
                    a: 255,
                }),
            );
        }
        // <code> standalone (not the `<pre><code>` block form) gets the
        // GitHub-style inline pill: monospace family + faint #f6f8fa
        // background + small horizontal padding + softly rounded corners.
        // The same chrome on a `<code>` inside `<pre>` would double up on
        // the `<pre>` panel and force redundant padding into the wrapped
        // text, so we suppress the pill in that case and let the parent
        // panel carry the look.
        "code" => {
            values.insert("font-family".into(), Value::Keyword("monospace".into()));
            if !has_pre_ancestor(document, node_id) {
                values.insert(
                    "background-color".into(),
                    Value::Color(crate::css::Color {
                        r: 246,
                        g: 248,
                        b: 250,
                        a: 255,
                    }),
                );
                // Padding is sized in `em` so the pill scales with the
                // surrounding font-size (10pt body keeps a tight pill,
                // a 24px heading gets a chunkier one). The cascade pass
                // resolves the em values to Px alongside the rest of
                // the descendant lengths.
                values.insert(
                    "padding-top".into(),
                    Value::Length(0.1, crate::css::Unit::Em),
                );
                values.insert(
                    "padding-bottom".into(),
                    Value::Length(0.1, crate::css::Unit::Em),
                );
                values.insert(
                    "padding-left".into(),
                    Value::Length(0.3, crate::css::Unit::Em),
                );
                values.insert(
                    "padding-right".into(),
                    Value::Length(0.3, crate::css::Unit::Em),
                );
                for side in ["top-left", "top-right", "bottom-right", "bottom-left"] {
                    values.insert(
                        format!("border-{side}-radius"),
                        Value::Length(3.0, crate::css::Unit::Px),
                    );
                }
            }
        }
        // The other monospace phrasing tags get only the family signal —
        // their visual shape is closer to plain inline text than to a
        // pill, and bundling chrome would visually compete with the
        // surrounding paragraph.
        "kbd" | "samp" | "tt" => {
            values.insert("font-family".into(), Value::Keyword("monospace".into()));
        }
        "input" | "textarea" => {
            values.insert(
                "display".into(),
                Value::Keyword("inline-block".into()),
            );
            values.insert(
                "width".into(),
                Value::Length(200.0, crate::css::Unit::Px),
            );
            values.insert(
                "background-color".into(),
                Value::Color(crate::css::Color {
                    r: 255,
                    g: 255,
                    b: 255,
                    a: 255,
                }),
            );
            values.insert(
                "color".into(),
                Value::Color(crate::css::Color {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 255,
                }),
            );
            edge_defaults(&mut values, "border", 1.0);
            values.insert(
                "border-color".into(),
                Value::Color(crate::css::Color {
                    r: 118,
                    g: 118,
                    b: 118,
                    a: 255,
                }),
            );
            values.insert(
                "padding-left".into(),
                Value::Length(4.0, crate::css::Unit::Px),
            );
            values.insert(
                "padding-right".into(),
                Value::Length(4.0, crate::css::Unit::Px),
            );
            values.insert(
                "padding-top".into(),
                Value::Length(2.0, crate::css::Unit::Px),
            );
            values.insert(
                "padding-bottom".into(),
                Value::Length(2.0, crate::css::Unit::Px),
            );
        }
        _ => {}
    }

    values
}

fn edge_defaults(values: &mut PropertyMap, prefix: &str, amount: f32) {
    for side in ["top", "right", "bottom", "left"] {
        values.insert(
            format!("{prefix}-{side}"),
            Value::Length(amount, crate::css::Unit::Px),
        );
    }
}

// True when any ancestor of `node_id` (exclusive) is a `<pre>` element.
// Used by the inline `<code>` UA default to suppress its pill chrome
// when the `<code>` is already sitting inside a `<pre>` block — the
// pre panel carries the look and a second one would double up.
fn has_pre_ancestor(document: &Document, node_id: NodeId) -> bool {
    let mut current = document.get(node_id).and_then(|n| n.parent);
    while let Some(parent_id) = current {
        match document.get(parent_id).map(|n| &n.node_type) {
            Some(NodeType::Element(element))
                if element.tag_name.eq_ignore_ascii_case("pre") =>
            {
                return true;
            }
            _ => {}
        }
        current = document.get(parent_id).and_then(|n| n.parent);
    }
    false
}
