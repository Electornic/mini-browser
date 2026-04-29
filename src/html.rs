use crate::dom::{AttrMap, Node};

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

pub fn parse(source: &str) -> Result<Vec<Node>, ParseError> {
    let mut parser = Parser::new(source);
    let nodes = parser.parse_nodes()?;
    parser.consume_whitespace();

    // If anything is left after parsing sibling nodes, we treat it as malformed input.
    if parser.eof() {
        Ok(nodes)
    } else {
        Err(ParseError::new(
            parser.pos,
            "unexpected trailing input after parsing document",
        ))
    }
}

struct Parser<'a> {
    pos: usize,
    input: &'a str,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self { pos: 0, input }
    }

    fn parse_nodes(&mut self) -> Result<Vec<Node>, ParseError> {
        let mut nodes = Vec::new();

        loop {
            self.consume_whitespace();

            if self.eof() || self.starts_with("</") {
                break;
            }

            // Repeatedly parse siblings until a closing tag or end-of-input ends this level.
            let node = self.parse_node()?;
            if !matches!(node.node_type, crate::dom::NodeType::Text(ref text) if text.is_empty()) {
                nodes.push(node);
            }
        }

        Ok(nodes)
    }

    fn parse_node(&mut self) -> Result<Node, ParseError> {
        // A leading '<' means element syntax; anything else is raw text content.
        if self.next_char() == Some('<') {
            // Skip constructs the toy parser does not model but real HTML contains.
            if self.starts_with("<!") {
                self.skip_declaration_or_comment();
                return Ok(Node::text(""));
            }
            self.parse_element()
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

    fn parse_element(&mut self) -> Result<Node, ParseError> {
        self.expect_char('<')?;
        let tag_name = self.parse_tag_name()?;
        let attributes = self.parse_attributes()?;

        // Self-closing elements stop immediately and never recurse into children.
        if self.starts_with("/>") {
            self.pos += 2;
            return Ok(Node::element(tag_name, attributes, Vec::new()));
        }

        self.expect_char('>')?;

        // Void elements never have children or closing tags in real HTML.
        if is_void_element(&tag_name) {
            return Ok(Node::element(tag_name, attributes, Vec::new()));
        }

        // Raw text elements contain code or styles, not nested HTML.
        // Their content must be consumed verbatim until the matching closing tag.
        if is_raw_text_element(&tag_name) {
            self.skip_raw_text_content(&tag_name);
            return Ok(Node::element(tag_name, attributes, Vec::new()));
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

        Ok(Node::element(tag_name, attributes, children))
    }

    fn skip_raw_text_content(&mut self, tag_name: &str) {
        let closing = format!("</{tag_name}>");
        while !self.eof() && !self.starts_with_ignore_case(&closing) {
            // Advance by one full character to stay on valid UTF-8 boundaries.
            if let Some(ch) = self.next_char() {
                self.pos += ch.len_utf8();
            }
        }
        // Consume the closing tag itself.
        if self.starts_with_ignore_case(&closing) {
            self.pos += closing.len();
        }
    }

    fn starts_with_ignore_case(&self, value: &str) -> bool {
        self.input[self.pos..]
            .get(..value.len())
            .is_some_and(|slice| slice.eq_ignore_ascii_case(value))
    }

    fn parse_text(&mut self) -> Node {
        // Text is everything until the next '<'.
        let text = self.consume_while(|ch| ch != '<');
        Node::text(text)
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

fn is_void_element(tag_name: &str) -> bool {
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
        let nodes = parse("<div id='root'><p>Hello</p><span>world</span></div>").unwrap();

        assert_eq!(nodes.len(), 1);

        let root = &nodes[0];
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

        let paragraph = &root.children[0];
        let paragraph_element = match &paragraph.node_type {
            NodeType::Element(data) => data,
            NodeType::Text(_) => panic!("expected paragraph element"),
        };
        assert_eq!(paragraph_element.tag_name, "p");
        assert_eq!(paragraph.children, vec![crate::dom::Node::text("Hello")]);
    }

    #[test]
    fn parses_multiple_attributes_with_mixed_quotes() {
        let nodes = parse(r#"<img src="hero.png" alt='Hero' data-id=abc />"#).unwrap();

        assert_eq!(nodes.len(), 1);
        let element = match &nodes[0].node_type {
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
