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
        let roots = parser.parse_nodes()?;
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
        let roots = parser.parse_nodes()?;
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

    fn parse_nodes(&mut self) -> Result<Vec<NodeId>, ParseError> {
        let mut nodes = Vec::new();

        loop {
            self.consume_whitespace();

            if self.eof() || self.starts_with("</") {
                break;
            }

            // Repeatedly parse siblings until a closing tag or end-of-input ends this level.
            // `parse_node` returns `None` for skipped declarations / comments and for
            // empty text runs; those simply don't add a child.
            if let Some(id) = self.parse_node()? {
                nodes.push(id);
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

        let children = self.parse_nodes()?;

        // Keep balancing strict instead of trying to recover like a real browser would.
        self.expect_char('<')?;
        self.expect_char('/')?;
        let closing_tag = self.parse_tag_name()?;
        if closing_tag != tag_name {
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
        let text = self.consume_while(|ch| ch != '<');
        if text.is_empty() {
            None
        } else {
            Some(self.document.create_text(text))
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
                Ok(value)
            }
            // Unquoted attributes are supported because they are easy to handle and common in demos.
            Some(_) => Ok(self.consume_while(|ch| !ch.is_whitespace() && ch != '>' && ch != '/')),
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

/// Tags whose HTML serialization has no content and no closing tag (`<br>`,
/// `<img>`, `<input>`, …). Exposed for the JS `innerHTML` getter, which
/// emits an opening tag only for void elements and skips both the children
/// and the close — matching the HTML serialization spec.
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
    fn returns_error_for_mismatched_closing_tag() {
        let error = parse("<div><p>Hello</div>").unwrap_err();

        assert!(error.message.contains("mismatched closing tag"));
    }
}
