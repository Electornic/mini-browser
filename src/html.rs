use crate::dom::{AttrMap, Document, NodeId};

// This parser intentionally accepts only a small, well-formed subset of HTML.
// The goal is to turn HTML text into a DOM tree, not to reproduce full browser recovery rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub position: usize,
    pub message: String,
}

impl ParseError {
    fn new(position: usize, message: impl Into<String>) -> Self {
        Self {
            position,
            message: message.into(),
        }
    }
}

pub fn parse(source: &str) -> Result<Document, ParseError> {
    let mut document = Document::new();
    // The parser's `&mut Document` borrow has to be released before we re-touch
    // `document` (to append the parsed roots), so the parsing pass lives in its
    // own scope. Returning the trailing offsets out of the scope keeps the eof
    // check at the top level alongside the rest of the function's flow.
    let (roots, trailing_pos, trailing_eof) = {
        let mut parser = Parser::new(source, &mut document);
        let roots = parser.parse_nodes(None)?;
        parser.consume_whitespace();
        (roots, parser.pos, parser.eof())
    };

    // If anything is left after parsing sibling nodes, we treat it as malformed input.
    if !trailing_eof {
        return Err(ParseError::new(
            trailing_pos,
            "unexpected trailing input after parsing document",
        ));
    }

    for root in roots {
        document.append_root(root);
    }
    Ok(document)
}

/// Parse `source` as a fragment — zero or more sibling nodes — into the
/// existing `document`. Returns the freshly created top-level NodeIds; they
/// are detached (not in `document.roots()` and have no parent) so callers
/// can splice them under any existing element via `append_child`. Used by
/// the JS `innerHTML` setter to swap an element's children without
/// destroying the rest of the tree.
pub fn parse_fragment(
    source: &str,
    document: &mut Document,
) -> Result<Vec<NodeId>, ParseError> {
    let (roots, trailing_pos, trailing_eof) = {
        let mut parser = Parser::new(source, document);
        let roots = parser.parse_nodes(None)?;
        parser.consume_whitespace();
        (roots, parser.pos, parser.eof())
    };

    if !trailing_eof {
        return Err(ParseError::new(
            trailing_pos,
            "unexpected trailing input after parsing fragment",
        ));
    }

    Ok(roots)
}

struct Parser<'a, 'd> {
    pos: usize,
    input: &'a str,
    document: &'d mut Document,
}

impl<'a, 'd> Parser<'a, 'd> {
    fn new(input: &'a str, document: &'d mut Document) -> Self {
        Self {
            pos: 0,
            input,
            document,
        }
    }

    fn parse_nodes(&mut self, open_tag: Option<&str>) -> Result<Vec<NodeId>, ParseError> {
        let mut nodes = Vec::new();
        let mut had_node = false;

        loop {
            let ws_start = self.pos;
            self.consume_whitespace();

            if self.eof() || self.starts_with("</") {
                break;
            }

            // HTML5 implicit-close: an opening tag that auto-closes the
            // currently open element ends this child list early. The
            // ancestor's `parse_nodes` picks the opener up as its own
            // child, turning `<li>a<li>b</ul>` into two sibling list
            // items rather than nested ones. The opener is left in
            // `self.input` deliberately — we are *not* consuming it here.
            if let Some(parent) = open_tag
                && let Some(opener) = self.peek_opening_tag_name()
                && auto_closes(parent, &opener)
            {
                break;
            }

            // Whitespace that sits *between* two siblings (not at the start
            // or end of the parent) becomes a single-space text node so
            // inline runs like `<a>new</a> | <a>past</a>` keep the
            // separating space the author wrote. Without this, parsing
            // would fuse the two anchors into "new|past" and every
            // wrapping `<span>` of a real page (HN nav, story metadata)
            // collapses into one unbroken word. Block-level callers
            // (block layout) drop pure-whitespace text children so this
            // never inserts a vertical gap between block siblings.
            if had_node && self.pos > ws_start {
                let space = self.document.create_text(" ".to_string());
                nodes.push(space);
            }

            // Repeatedly parse siblings until a closing tag or end-of-input ends this level.
            // `parse_node` returns `None` for skipped declarations / comments and for
            // empty text runs; those simply don't add a child.
            if let Some(id) = self.parse_node()? {
                nodes.push(id);
                had_node = true;
            }
        }

        Ok(nodes)
    }

    fn parse_node(&mut self) -> Result<Option<NodeId>, ParseError> {
        // A leading '<' means element syntax; anything else is raw text content.
        if self.next_char() == Some('<') {
            // Skip constructs the toy parser does not model but real HTML contains.
            if self.starts_with("<!") {
                self.skip_declaration_or_comment();
                return Ok(None);
            }
            self.parse_element().map(Some)
        } else {
            Ok(self.parse_text())
        }
    }

    fn skip_declaration_or_comment(&mut self) {
        if self.starts_with("<!--") {
            // HTML comments end at the first "-->".
            self.pos += 4;
            while !self.eof() && !self.starts_with("-->") {
                if let Some(ch) = self.next_char() {
                    self.pos += ch.len_utf8();
                }
            }
            if self.starts_with("-->") {
                self.pos += 3;
            }
        } else {
            // DOCTYPE and other <! declarations end at '>'.
            while !self.eof() && self.next_char() != Some('>') {
                if let Some(ch) = self.next_char() {
                    self.pos += ch.len_utf8();
                }
            }
            if self.next_char() == Some('>') {
                self.pos += 1;
            }
        }
    }

    fn parse_element(&mut self) -> Result<NodeId, ParseError> {
        self.expect_char('<')?;
        let tag_name = self.parse_tag_name()?;
        let attributes = self.parse_attributes()?;

        // Self-closing elements stop immediately and never recurse into children.
        if self.starts_with("/>") {
            self.pos += 2;
            return Ok(self.document.create_element(tag_name, attributes));
        }

        self.expect_char('>')?;

        // Void elements never have children or closing tags in real HTML.
        if is_void_element(&tag_name) {
            return Ok(self.document.create_element(tag_name, attributes));
        }

        // Raw text elements contain code or styles, not nested HTML.
        // Their content is consumed verbatim until the matching closing tag and
        // preserved as a single text child so later stages (e.g. JS execution
        // for `<script>`) can read the source. Empty bodies still yield no
        // children to keep the tree compact.
        if is_raw_text_element(&tag_name) {
            let content = self.consume_raw_text_content(&tag_name);
            let element = self.document.create_element(tag_name, attributes);
            if !content.is_empty() {
                let text = self.document.create_text(content);
                self.document.append_child(element, text);
            }
            return Ok(element);
        }

        let children = self.parse_nodes(Some(&tag_name))?;

        // HTML5 implicit-close: parse_nodes stops when it hits any closing
        // tag *or* an opening tag that auto-closes us. If what's next is
        // not our own `</tag>`, leave it in the input — the ancestor will
        // see it and decide. This turns omitted `</li>` / `</p>` / `</tr>`
        // / mismatched closers into well-formed siblings instead of hard
        // errors, matching how every real browser is forced to cope.
        if !self.is_closing_tag_for(&tag_name) {
            let element = self.document.create_element(tag_name, attributes);
            for child in children {
                self.document.append_child(element, child);
            }
            return Ok(element);
        }

        self.expect_char('<')?;
        self.expect_char('/')?;
        let closing_tag = self.parse_tag_name()?;
        if !closing_tag.eq_ignore_ascii_case(&tag_name) {
            return Err(ParseError::new(
                self.pos,
                format!("mismatched closing tag: expected </{tag_name}>"),
            ));
        }
        self.consume_whitespace();
        self.expect_char('>')?;

        let element = self.document.create_element(tag_name, attributes);
        for child in children {
            self.document.append_child(element, child);
        }
        Ok(element)
    }

    fn consume_raw_text_content(&mut self, tag_name: &str) -> String {
        let closing = format!("</{tag_name}>");
        let start = self.pos;
        while !self.eof() && !self.starts_with_ignore_case(&closing) {
            // Advance by one full character to stay on valid UTF-8 boundaries.
            if let Some(ch) = self.next_char() {
                self.pos += ch.len_utf8();
            }
        }
        let content = self.input[start..self.pos].to_string();
        // Consume the closing tag itself.
        if self.starts_with_ignore_case(&closing) {
            self.pos += closing.len();
        }
        content
    }

    fn starts_with_ignore_case(&self, value: &str) -> bool {
        self.input[self.pos..]
            .get(..value.len())
            .is_some_and(|slice| slice.eq_ignore_ascii_case(value))
    }

    fn parse_text(&mut self) -> Option<NodeId> {
        // Text is everything until the next '<'. Empty runs (which can appear
        // when whitespace was just consumed or `<!--…-->` was skipped) produce
        // no node so the resulting tree stays free of zero-length text leaves.
        let raw = self.consume_while(|ch| ch != '<');
        if raw.is_empty() {
            None
        } else {
            Some(self.document.create_text(decode_entities(&raw)))
        }
    }

    fn parse_attributes(&mut self) -> Result<AttrMap, ParseError> {
        let mut attributes = AttrMap::new();

        loop {
            self.consume_whitespace();

            if self.eof() || self.starts_with(">") || self.starts_with("/>") {
                break;
            }

            // Duplicate keys simply overwrite earlier values, which is good enough here.
            let (name, value) = self.parse_attr()?;
            attributes.insert(name, value);
        }

        Ok(attributes)
    }

    fn parse_attr(&mut self) -> Result<(String, String), ParseError> {
        let name = self.parse_tag_name()?;
        self.consume_whitespace();

        // Boolean attributes like `disabled` or `async` have no value.
        if self.next_char() != Some('=') {
            return Ok((name, String::new()));
        }

        self.expect_char('=')?;
        self.consume_whitespace();
        let value = self.parse_attr_value()?;
        Ok((name, value))
    }

    fn parse_attr_value(&mut self) -> Result<String, ParseError> {
        match self.next_char() {
            Some('"') | Some('\'') => {
                let quote = self.consume_char().expect("quote checked above");
                let value = self.consume_while(|ch| ch != quote);
                self.expect_char(quote)?;
                Ok(decode_entities(&value))
            }
            // Unquoted attributes are supported because they are easy to handle and common in demos.
            Some(_) => Ok(decode_entities(
                &self.consume_while(|ch| !ch.is_whitespace() && ch != '>' && ch != '/'),
            )),
            None => Err(ParseError::new(
                self.pos,
                "unexpected end of input while parsing attribute value",
            )),
        }
    }

    fn parse_tag_name(&mut self) -> Result<String, ParseError> {
        let name = self.consume_while(|ch| ch.is_alphanumeric() || ch == '-' || ch == '_');
        if name.is_empty() {
            Err(ParseError::new(self.pos, "expected tag or attribute name"))
        } else {
            Ok(name)
        }
    }

    fn consume_whitespace(&mut self) {
        self.consume_while(char::is_whitespace);
    }

    fn consume_while<F>(&mut self, test: F) -> String
    where
        F: Fn(char) -> bool,
    {
        let mut result = String::new();

        // The parser is character-based, so every helper advances `pos` as it consumes input.
        while let Some(ch) = self.next_char() {
            if !test(ch) {
                break;
            }
            result.push(self.consume_char().expect("character must exist"));
        }

        result
    }

    fn starts_with(&self, value: &str) -> bool {
        self.input[self.pos..].starts_with(value)
    }

    // Look ahead at `<tag…` (or `<tag/`, `<tag>`) without advancing `pos`.
    // Returns the lowercased tag name when the cursor is at an opening tag,
    // None for closing tags, declarations/comments, raw text, or end-of-input.
    // Used by the implicit-close path so a `<li>…<li>` author actually gets
    // siblings instead of nested elements (see `auto_closes`).
    fn peek_opening_tag_name(&self) -> Option<String> {
        if !self.starts_with("<") || self.starts_with("</") || self.starts_with("<!") {
            return None;
        }
        let mut tag = String::new();
        for ch in self.input[self.pos + 1..].chars() {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                tag.push(ch);
            } else {
                break;
            }
        }
        if tag.is_empty() {
            None
        } else {
            Some(tag.to_ascii_lowercase())
        }
    }

    // True iff the cursor sits on `</name>` (or `</name…`) ignoring case and
    // requiring a non-name boundary character after the name. Lets
    // `parse_element` distinguish "this is *my* closing tag, consume it"
    // from "this is an ancestor's closing tag, leave it for them".
    fn is_closing_tag_for(&self, name: &str) -> bool {
        if !self.starts_with("</") {
            return false;
        }
        let after = &self.input[self.pos + 2..];
        let prefix = match after.get(..name.len()) {
            Some(slice) => slice,
            None => return false,
        };
        if !prefix.eq_ignore_ascii_case(name) {
            return false;
        }
        // Boundary: the next char must not extend the name (so `</p>` does
        // not match `is_closing_tag_for("pa")`).
        match after.as_bytes().get(name.len()).copied() {
            None => true,
            Some(b) => !(b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        }
    }

    fn eof(&self) -> bool {
        self.pos >= self.input.len()
    }

    fn next_char(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn consume_char(&mut self) -> Option<char> {
        let current = self.next_char()?;
        self.pos += current.len_utf8();
        Some(current)
    }

    fn expect_char(&mut self, expected: char) -> Result<(), ParseError> {
        match self.consume_char() {
            Some(actual) if actual == expected => Ok(()),
            Some(actual) => Err(ParseError::new(
                self.pos,
                format!("expected '{expected}', found '{actual}'"),
            )),
            None => Err(ParseError::new(
                self.pos,
                format!("expected '{expected}', found end of input"),
            )),
        }
    }
}

fn is_raw_text_element(tag_name: &str) -> bool {
    matches!(tag_name, "script" | "style")
}

/// Decode HTML character references inside a text run or attribute value.
/// Handles named entities (`&amp;`, `&nbsp;`, …), decimal numeric (`&#39;`),
/// and hex numeric (`&#x27;`). An entity is recognized only when a `;`
/// terminator appears within a small window and the body is non-empty and
/// contains no whitespace, `<`, or stray `&`. Anything else — bad bodies,
/// unknown names, missing semicolons — is left verbatim, matching real
/// browsers' permissive policy on broken markup.
fn decode_entities(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }
    // Longest entity reference we model is `&#x10FFFF;` (10 chars). 16 leaves
    // headroom for slightly-longer named entities while still bailing fast on
    // a stray '&' in normal prose.
    const MAX_LOOKAHEAD: usize = 16;
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let ch = input[i..]
            .chars()
            .next()
            .expect("loop condition guarantees a char");
        let len = ch.len_utf8();
        if ch != '&' {
            out.push(ch);
            i += len;
            continue;
        }
        let look_end = (i + 1 + MAX_LOOKAHEAD).min(input.len());
        let slice = &input[i + 1..look_end];
        let Some(semi) = slice.find(';') else {
            out.push('&');
            i += 1;
            continue;
        };
        let body = &slice[..semi];
        if body.is_empty()
            || body
                .chars()
                .any(|c| c.is_whitespace() || c == '<' || c == '&')
        {
            out.push('&');
            i += 1;
            continue;
        }
        match decode_one_entity(body) {
            Some(decoded) => {
                out.push_str(&decoded);
                i += 1 + body.len() + 1; // skip past `&body;`
            }
            None => {
                out.push('&');
                i += 1;
            }
        }
    }
    out
}

fn decode_one_entity(body: &str) -> Option<String> {
    if let Some(num) = body.strip_prefix('#') {
        let codepoint = if let Some(hex) = num.strip_prefix(['x', 'X']) {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            num.parse::<u32>().ok()?
        };
        char::from_u32(codepoint).map(|ch| ch.to_string())
    } else {
        named_entity(body).map(|s| s.to_string())
    }
}

fn named_entity(name: &str) -> Option<&'static str> {
    Some(match name {
        // Core five — the only ones that are "must" in HTML serialization.
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        // Whitespace + typography most commonly seen on real pages.
        "nbsp" => "\u{00A0}",
        "ensp" => "\u{2002}",
        "emsp" => "\u{2003}",
        "thinsp" => "\u{2009}",
        // Punctuation / dashes / quotes.
        "ndash" => "\u{2013}",
        "mdash" => "\u{2014}",
        "lsquo" => "\u{2018}",
        "rsquo" => "\u{2019}",
        "ldquo" => "\u{201C}",
        "rdquo" => "\u{201D}",
        "laquo" => "\u{00AB}",
        "raquo" => "\u{00BB}",
        "hellip" => "\u{2026}",
        "middot" => "\u{00B7}",
        "bull" => "\u{2022}",
        // Common symbols.
        "copy" => "\u{00A9}",
        "reg" => "\u{00AE}",
        "trade" => "\u{2122}",
        "deg" => "\u{00B0}",
        "plusmn" => "\u{00B1}",
        "times" => "\u{00D7}",
        "divide" => "\u{00F7}",
        // Math / arrows.
        "larr" => "\u{2190}",
        "uarr" => "\u{2191}",
        "rarr" => "\u{2192}",
        "darr" => "\u{2193}",
        _ => return None,
    })
}

/// Tags whose HTML serialization has no content and no closing tag (`<br>`,
/// `<img>`, `<input>`, …). Exposed for the JS `innerHTML` getter, which
/// emits an opening tag only for void elements and skips both the children
/// and the close — matching the HTML serialization spec.
// HTML5 implicit-close table. Returns true when seeing `<opener>` while
// `parent` is still open should close `parent` first instead of nesting.
//
// Real browsers express this via the open-elements stack and per-element
// "in scope" rules; here we keep it as a flat per-pair lookup because the
// parser is recursion-based and only needs to decide one level at a time.
// Small, principled subset: the cases real-world pages most often rely on
// (omitted `</li>` / `</p>` / `</tr>` / `</td>`, table section swaps,
// option/optgroup siblings).
pub fn auto_closes(parent: &str, opener: &str) -> bool {
    // Tag names are matched case-insensitively to mirror HTML's normalisation
    // (which our DOM doesn't apply at create time).
    let parent = parent.to_ascii_lowercase();
    let opener = opener.to_ascii_lowercase();
    match parent.as_str() {
        "li" => opener == "li",
        "dt" | "dd" => opener == "dt" || opener == "dd",
        "tr" => opener == "tr",
        "td" | "th" => opener == "td" || opener == "th",
        "tbody" | "thead" | "tfoot" => matches!(opener.as_str(), "tbody" | "thead" | "tfoot"),
        "option" => opener == "option" || opener == "optgroup",
        "optgroup" => opener == "optgroup",
        // <p> doesn't allow block-level descendants — any block opener
        // implicitly closes the open <p>. The list mirrors the spec's
        // "in button scope" set restricted to elements that exist in the
        // small layout we support.
        "p" => is_p_closing_block_opener(&opener),
        _ => false,
    }
}

fn is_p_closing_block_opener(tag: &str) -> bool {
    matches!(
        tag,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "details"
            | "div"
            | "dl"
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
            | "hgroup"
            | "hr"
            | "main"
            | "menu"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "table"
            | "ul"
    )
}

pub fn is_void_element(tag_name: &str) -> bool {
    matches!(
        tag_name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

#[cfg(test)]
mod tests {
    use crate::dom::NodeType;

    use super::parse;

    #[test]
    fn parses_nested_elements_and_text() {
        let document = parse("<div id='root'><p>Hello</p><span>world</span></div>").unwrap();

        assert_eq!(document.roots().len(), 1);

        let root_id = document.roots()[0];
        let root = document.get(root_id).unwrap();
        let root_element = match &root.node_type {
            NodeType::Element(data) => data,
            NodeType::Text(_) => panic!("expected element node"),
        };

        assert_eq!(root_element.tag_name, "div");
        assert_eq!(
            root_element.attributes.get("id").map(String::as_str),
            Some("root")
        );
        assert_eq!(root.children.len(), 2);

        let paragraph = document.get(root.children[0]).unwrap();
        let paragraph_element = match &paragraph.node_type {
            NodeType::Element(data) => data,
            NodeType::Text(_) => panic!("expected paragraph element"),
        };
        assert_eq!(paragraph_element.tag_name, "p");
        assert_eq!(paragraph.children.len(), 1);
        assert_eq!(document.text(paragraph.children[0]), Some("Hello"));
    }

    #[test]
    fn preserves_whitespace_between_sibling_elements_as_single_space_text() {
        // HN's nav bar separates anchors with `<a>new</a> | <a>past</a>`
        // and the menu falls apart visually if the space text disappears
        // on parse. The parser injects a single-space text node between
        // any two adjacent siblings whose source had whitespace between
        // them — even when the whitespace was newlines and indent.
        let document = parse("<span><a>x</a>\n  <a>y</a></span>").unwrap();
        let root = document.get(document.roots()[0]).unwrap();
        // Children: <a>x</a>, " ", <a>y</a> — three nodes total.
        assert_eq!(root.children.len(), 3);
        let middle = document.text(root.children[1]);
        assert_eq!(middle, Some(" "));
    }

    #[test]
    fn does_not_inject_whitespace_at_start_or_end_of_a_parent() {
        // Whitespace before the first sibling and after the last one
        // collapses away — only inter-sibling gaps produce a text node.
        // Without this rule, every nested element would gain a stray
        // leading/trailing space and the box-tree would be cluttered.
        let document = parse("<span>  <a>x</a>  </span>").unwrap();
        let root = document.get(document.roots()[0]).unwrap();
        // Just the single anchor child; leading and trailing whitespace gone.
        assert_eq!(root.children.len(), 1);
    }

    #[test]
    fn parses_multiple_attributes_with_mixed_quotes() {
        let document = parse(r#"<img src="hero.png" alt='Hero' data-id=abc />"#).unwrap();

        assert_eq!(document.roots().len(), 1);
        let root_id = document.roots()[0];
        let element = match &document.get(root_id).unwrap().node_type {
            NodeType::Element(data) => data,
            NodeType::Text(_) => panic!("expected element node"),
        };

        assert_eq!(element.tag_name, "img");
        assert_eq!(
            element.attributes.get("src").map(String::as_str),
            Some("hero.png")
        );
        assert_eq!(
            element.attributes.get("alt").map(String::as_str),
            Some("Hero")
        );
        assert_eq!(
            element.attributes.get("data-id").map(String::as_str),
            Some("abc")
        );
    }

    #[test]
    fn implicitly_closes_inner_element_on_outer_closer() {
        // `<div><p>Hello</div>` was treated as a hard error before HTML5
        // implicit-close support landed; now the unmatched `</div>` closes
        // the open `<p>` first and is consumed as the `<div>`'s own closer.
        // The resulting tree is what every real browser produces.
        let document = parse("<div><p>Hello</div>").unwrap();
        let div = document.roots()[0];
        let div_node = document.get(div).unwrap();
        let crate::dom::NodeType::Element(div_elem) = &div_node.node_type else {
            panic!("root must be an element");
        };
        assert_eq!(div_elem.tag_name, "div");
        assert_eq!(div_node.children.len(), 1);

        let p = div_node.children[0];
        let p_node = document.get(p).unwrap();
        let crate::dom::NodeType::Element(p_elem) = &p_node.node_type else {
            panic!("expected <p> as div's child");
        };
        assert_eq!(p_elem.tag_name, "p");

        let text_id = p_node.children[0];
        assert_eq!(document.text(text_id), Some("Hello"));
    }

    #[test]
    fn implicitly_closes_li_when_sibling_li_opens() {
        // Real-world lists usually omit `</li>`; the previous strict parser
        // produced a single nested `<li>` containing the second one. Under
        // HTML5 implicit-close they end up as siblings of the `<ul>`.
        let document = parse("<ul><li>a<li>b</ul>").unwrap();
        let ul = document.roots()[0];
        let ul_node = document.get(ul).unwrap();
        // Two <li> children — the implicit close means b is a sibling of a.
        let lis: Vec<_> = ul_node
            .children
            .iter()
            .filter_map(|id| {
                let node = document.get(*id)?;
                match &node.node_type {
                    crate::dom::NodeType::Element(el) if el.tag_name == "li" => Some(*id),
                    _ => None,
                }
            })
            .collect();
        assert_eq!(lis.len(), 2);
        let first = document.get(lis[0]).unwrap().children[0];
        let second = document.get(lis[1]).unwrap().children[0];
        assert_eq!(document.text(first), Some("a"));
        assert_eq!(document.text(second), Some("b"));
    }

    #[test]
    fn implicitly_closes_p_when_block_opener_starts() {
        // `<p>` cannot legally contain a block-level element — opening one
        // closes the paragraph first. After the change, `<p>foo<div>bar</div>`
        // produces two siblings (`<p>foo</p>` then `<div>bar</div>`),
        // which matches what every real browser does.
        let document = parse("<p>foo<div>bar</div>").unwrap();
        let roots = document.roots();
        // <p> and <div> are siblings under the document root.
        assert_eq!(roots.len(), 2);
        let p_tag = match &document.get(roots[0]).unwrap().node_type {
            crate::dom::NodeType::Element(e) => e.tag_name.clone(),
            _ => panic!("first root must be element"),
        };
        let div_tag = match &document.get(roots[1]).unwrap().node_type {
            crate::dom::NodeType::Element(e) => e.tag_name.clone(),
            _ => panic!("second root must be element"),
        };
        assert_eq!(p_tag, "p");
        assert_eq!(div_tag, "div");

        let p_text = document.get(roots[0]).unwrap().children[0];
        assert_eq!(document.text(p_text), Some("foo"));
    }

    #[test]
    fn implicitly_closes_table_cells_and_rows_when_closer_missing() {
        // `<table><tr><td>x</tr></table>` omits `</td>`; the implicit close
        // collapses out at the right level so the tree stays well-shaped:
        // table → tr → td → "x".
        let document = parse("<table><tr><td>x</tr></table>").unwrap();
        let table = document.roots()[0];
        let table_node = document.get(table).unwrap();
        // Find the <tr> under <table> (the parser may also emit
        // whitespace text — pick the first element child).
        let tr = *table_node
            .children
            .iter()
            .find(|id| {
                matches!(
                    document.get(**id).map(|n| &n.node_type),
                    Some(crate::dom::NodeType::Element(e)) if e.tag_name == "tr"
                )
            })
            .expect("tr child must exist");
        let tr_node = document.get(tr).unwrap();
        let td = *tr_node
            .children
            .iter()
            .find(|id| {
                matches!(
                    document.get(**id).map(|n| &n.node_type),
                    Some(crate::dom::NodeType::Element(e)) if e.tag_name == "td"
                )
            })
            .expect("td child must exist");
        let text = document.get(td).unwrap().children[0];
        assert_eq!(document.text(text), Some("x"));
    }

    #[test]
    fn returns_error_for_stray_closing_tag_at_top_level() {
        // Stray closers with no opener still error — implicit-close only
        // unwinds *open* elements. `</foo>` outside any element drops out
        // of every parse_nodes loop and gets surfaced by the trailing-input
        // check in `parse()`.
        let error = parse("</span>").unwrap_err();
        assert!(
            error.message.contains("trailing input"),
            "expected trailing-input error, got: {}",
            error.message,
        );
    }

    #[test]
    fn decodes_named_and_numeric_entities_in_text() {
        // The big five plus a typography sample (`&hellip;`), plus decimal
        // (`&#39;` apostrophe) and hex (`&#x27;` apostrophe) numeric forms —
        // both the HN comment thread and most blog markup hit this surface.
        let document = parse(
            "<p>&amp;&lt;&gt;&quot;&#39;&#x27;&hellip;&nbsp;&copy;</p>",
        )
        .unwrap();
        let p = document.roots()[0];
        let text_id = document.get(p).unwrap().children[0];
        assert_eq!(
            document.text(text_id),
            Some("&<>\"\'\'\u{2026}\u{00A0}\u{00A9}")
        );
    }

    #[test]
    fn keeps_unknown_or_malformed_entities_verbatim() {
        // Real pages contain stray `&` in prose ("Tom & Jerry"). Our policy
        // matches browsers: only known entity forms decode; everything else
        // — unknown names, missing `;`, embedded whitespace — stays literal.
        let document = parse("<p>Tom &amp; Jerry &unknown; & loose</p>").unwrap();
        let p = document.roots()[0];
        let text_id = document.get(p).unwrap().children[0];
        assert_eq!(
            document.text(text_id),
            Some("Tom & Jerry &unknown; & loose")
        );
    }

    #[test]
    fn decodes_entities_inside_attribute_values() {
        // Query strings frequently encode `&` as `&amp;` so the HTML stays
        // well-formed; the live attribute must be the decoded form so links
        // work and JS comparisons against the URL match the source.
        let document = parse(r#"<a href="?x=1&amp;y=2&#x3D;ok">go</a>"#).unwrap();
        let a = document.roots()[0];
        let element = match &document.get(a).unwrap().node_type {
            NodeType::Element(e) => e,
            _ => panic!("expected <a>"),
        };
        assert_eq!(
            element.attributes.get("href").map(String::as_str),
            Some("?x=1&y=2=ok")
        );
    }

    #[test]
    fn does_not_decode_entities_inside_script_or_style_bodies() {
        // `<script>` / `<style>` are raw-text elements: their body is consumed
        // verbatim until the closing tag, never re-tokenized for entities.
        // A JS comparison like `if (a < b)` must round-trip with `&lt;` left
        // alone — otherwise the script becomes a syntax error.
        let document =
            parse("<script>if (a&lt;b) {}</script>").unwrap();
        let script = document.roots()[0];
        let body = document.get(script).unwrap().children[0];
        assert_eq!(document.text(body), Some("if (a&lt;b) {}"));
    }
}
