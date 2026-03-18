use crate::dom::{AttrMap, Node};

// The HTML parser accepts a deliberately small, well-formed subset of HTML.
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

            // Parsing returns sibling nodes until a closing tag or end-of-input stops the loop.
            let node = self.parse_node()?;
            if !matches!(node.node_type, crate::dom::NodeType::Text(ref text) if text.is_empty()) {
                nodes.push(node);
            }
        }

        Ok(nodes)
    }

    fn parse_node(&mut self) -> Result<Node, ParseError> {
        if self.next_char() == Some('<') {
            self.parse_element()
        } else {
            Ok(self.parse_text())
        }
    }

    fn parse_element(&mut self) -> Result<Node, ParseError> {
        self.expect_char('<')?;
        let tag_name = self.parse_tag_name()?;
        let attributes = self.parse_attributes()?;

        if self.starts_with("/>") {
            self.pos += 2;
            return Ok(Node::element(tag_name, attributes, Vec::new()));
        }

        self.expect_char('>')?;
        let children = self.parse_nodes()?;

        // This parser keeps tag balancing strict instead of trying to recover like a real browser.
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

    fn parse_text(&mut self) -> Node {
        // Text is everything up to the next tag boundary.
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

            let (name, value) = self.parse_attr()?;
            attributes.insert(name, value);
        }

        Ok(attributes)
    }

    fn parse_attr(&mut self) -> Result<(String, String), ParseError> {
        let name = self.parse_tag_name()?;
        self.consume_whitespace();
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
