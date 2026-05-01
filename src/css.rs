// CSS support is intentionally narrow: simple selectors and a handful of value types.
// That keeps the parser small while still giving the rest of the browser realistic input.
#[derive(Debug, Clone, PartialEq)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub selectors: Vec<Selector>,
    pub declarations: Vec<Declaration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    Tag(String),
    Class(String),
    Id(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    pub name: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Keyword(String),
    Length(f32, Unit),
    Color(Color),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Px,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

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

pub fn parse(source: &str) -> Result<Stylesheet, ParseError> {
    let mut parser = Parser::new(source);
    let stylesheet = parser.parse_stylesheet_tolerant();
    Ok(stylesheet)
}

struct Parser<'a> {
    pos: usize,
    input: &'a str,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self { pos: 0, input }
    }

    fn parse_stylesheet_tolerant(&mut self) -> Stylesheet {
        let mut rules = Vec::new();

        loop {
            self.skip_whitespace_and_comments();

            if self.eof() {
                break;
            }

            // Skip at-rules (@media, @charset, @keyframes, etc.) the toy parser does not model.
            if self.next_char() == Some('@') {
                self.skip_at_rule();
                continue;
            }

            // Try parsing a normal rule; skip to the next block boundary on failure.
            let saved = self.pos;
            match self.parse_rule() {
                Ok(rule) => rules.push(rule),
                Err(_) => {
                    self.pos = saved;
                    self.skip_to_end_of_block();
                }
            }
        }

        Stylesheet { rules }
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            self.consume_whitespace();
            if self.starts_with("/*") {
                self.pos += 2;
                while !self.eof() && !self.starts_with("*/") {
                    if let Some(ch) = self.next_char() {
                        self.pos += ch.len_utf8();
                    }
                }
                if self.starts_with("*/") {
                    self.pos += 2;
                }
            } else {
                break;
            }
        }
    }

    fn skip_at_rule(&mut self) {
        // At-rules either end with ';' or contain a '{...}' block.
        let mut brace_depth = 0;
        while let Some(ch) = self.next_char() {
            self.pos += ch.len_utf8();
            match ch {
                '{' => brace_depth += 1,
                '}' if brace_depth > 1 => brace_depth -= 1,
                '}' if brace_depth == 1 => return,
                ';' if brace_depth == 0 => return,
                _ => {}
            }
        }
    }

    fn skip_to_end_of_block(&mut self) {
        // Advance past the next '}' so the parser can attempt the next rule.
        let mut brace_depth = 0;
        while let Some(ch) = self.next_char() {
            self.pos += ch.len_utf8();
            match ch {
                '{' => brace_depth += 1,
                '}' if brace_depth > 1 => brace_depth -= 1,
                '}' => return,
                _ => {}
            }
        }
    }

    fn starts_with(&self, value: &str) -> bool {
        self.input[self.pos..].starts_with(value)
    }

    fn parse_rule(&mut self) -> Result<Rule, ParseError> {
        let selectors = self.parse_selectors()?;
        self.consume_whitespace();
        self.expect_char('{')?;
        let declarations = self.parse_declarations()?;
        self.expect_char('}')?;

        Ok(Rule {
            selectors,
            declarations,
        })
    }

    fn parse_selectors(&mut self) -> Result<Vec<Selector>, ParseError> {
        let mut selectors = Vec::new();

        loop {
            self.skip_whitespace_and_comments();
            selectors.push(self.parse_selector()?);
            self.skip_whitespace_and_comments();

            // A single rule can target multiple simple selectors separated by commas.
            if self.next_char() == Some(',') {
                self.consume_char();
                continue;
            }

            break;
        }

        Ok(selectors)
    }

    fn parse_selector(&mut self) -> Result<Selector, ParseError> {
        match self.next_char() {
            Some('.') => {
                self.consume_char();
                Ok(Selector::Class(self.parse_identifier()?))
            }
            Some('#') => {
                self.consume_char();
                Ok(Selector::Id(self.parse_identifier()?))
            }
            Some(_) => Ok(Selector::Tag(self.parse_identifier()?)),
            None => Err(ParseError::new(
                self.pos,
                "unexpected end of input while parsing selector",
            )),
        }
    }

    fn parse_declarations(&mut self) -> Result<Vec<Declaration>, ParseError> {
        let mut declarations = Vec::new();

        loop {
            self.skip_whitespace_and_comments();

            if self.eof() || self.next_char() == Some('}') {
                break;
            }

            // Try parsing a declaration; skip to the next ';' or '}' on failure.
            // Shorthands like `border-radius` expand into multiple per-corner declarations,
            // so each call may contribute more than one entry.
            let saved = self.pos;
            match self.parse_declaration() {
                Ok(decls) => declarations.extend(decls),
                Err(_) => {
                    self.pos = saved;
                    self.consume_while(|ch| ch != ';' && ch != '}');
                }
            }

            self.skip_whitespace_and_comments();
            if self.next_char() == Some(';') {
                self.consume_char();
            }
        }

        Ok(declarations)
    }

    fn parse_declaration(&mut self) -> Result<Vec<Declaration>, ParseError> {
        let name = self.parse_identifier()?;
        self.consume_whitespace();
        self.expect_char(':')?;
        self.consume_whitespace();

        if name == "border-radius" {
            return self.parse_border_radius_shorthand();
        }

        let value = self.parse_value()?;
        Ok(vec![Declaration { name, value }])
    }

    fn parse_border_radius_shorthand(&mut self) -> Result<Vec<Declaration>, ParseError> {
        // CSS allows 1-4 length tokens. Per spec: 1=all, 2=tl/br + tr/bl,
        // 3=tl + tr/bl + br, 4=tl + tr + br + bl.
        let mut lengths = Vec::new();
        loop {
            self.consume_whitespace();
            match self.next_char() {
                Some(ch) if ch.is_ascii_digit() || ch == '.' => {}
                _ => break,
            }
            lengths.push(self.parse_length_or_number()?);
            if lengths.len() == 4 {
                break;
            }
        }

        if lengths.is_empty() {
            return Err(ParseError::new(
                self.pos,
                "border-radius requires at least one length",
            ));
        }

        let (tl, tr, br, bl) = match lengths.as_slice() {
            [v] => (v.clone(), v.clone(), v.clone(), v.clone()),
            [tl_br, tr_bl] => (tl_br.clone(), tr_bl.clone(), tl_br.clone(), tr_bl.clone()),
            [tl, tr_bl, br] => (tl.clone(), tr_bl.clone(), br.clone(), tr_bl.clone()),
            [tl, tr, br, bl] => (tl.clone(), tr.clone(), br.clone(), bl.clone()),
            _ => unreachable!(),
        };

        Ok(vec![
            Declaration {
                name: "border-top-left-radius".into(),
                value: tl,
            },
            Declaration {
                name: "border-top-right-radius".into(),
                value: tr,
            },
            Declaration {
                name: "border-bottom-right-radius".into(),
                value: br,
            },
            Declaration {
                name: "border-bottom-left-radius".into(),
                value: bl,
            },
        ])
    }

    fn parse_value(&mut self) -> Result<Value, ParseError> {
        match self.next_char() {
            Some('#') => self.parse_hex_color(),
            Some(ch) if ch.is_ascii_digit() => self.parse_length_or_number(),
            // Everything else is treated as a keyword to keep the engine permissive.
            Some(_) => Ok(Value::Keyword(self.parse_identifier()?)),
            None => Err(ParseError::new(
                self.pos,
                "unexpected end of input while parsing value",
            )),
        }
    }

    fn parse_hex_color(&mut self) -> Result<Value, ParseError> {
        self.expect_char('#')?;
        let hex = self.consume_while(|ch| ch.is_ascii_hexdigit());

        let (r, g, b) = match hex.len() {
            3 => {
                let chars: Vec<char> = hex.chars().collect();
                (
                    Self::expand_hex(chars[0])?,
                    Self::expand_hex(chars[1])?,
                    Self::expand_hex(chars[2])?,
                )
            }
            6 => (
                Self::parse_hex_pair(&hex[0..2])?,
                Self::parse_hex_pair(&hex[2..4])?,
                Self::parse_hex_pair(&hex[4..6])?,
            ),
            _ => {
                return Err(ParseError::new(
                    self.pos,
                    "hex colors must use either 3 or 6 digits",
                ));
            }
        };

        Ok(Value::Color(Color { r, g, b, a: 255 }))
    }

    fn parse_length_or_number(&mut self) -> Result<Value, ParseError> {
        let number = self.consume_while(|ch| ch.is_ascii_digit() || ch == '.');
        let value = number.parse::<f32>().map_err(|_| {
            ParseError::new(
                self.pos,
                format!("invalid numeric value '{number}' in declaration"),
            )
        })?;

        let unit = self.parse_identifier()?;
        match unit.as_str() {
            "px" => Ok(Value::Length(value, Unit::Px)),
            // Unsupported units fall back to plain keywords instead of hard-failing.
            _ => Ok(Value::Keyword(format!("{value}{unit}"))),
        }
    }

    fn parse_identifier(&mut self) -> Result<String, ParseError> {
        let ident = self.consume_while(|ch| ch.is_alphanumeric() || ch == '-' || ch == '_');
        if ident.is_empty() {
            Err(ParseError::new(self.pos, "expected identifier"))
        } else {
            Ok(ident)
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

    fn parse_hex_pair(pair: &str) -> Result<u8, ParseError> {
        u8::from_str_radix(pair, 16)
            .map_err(|_| ParseError::new(0, format!("invalid hex color pair '{pair}'")))
    }

    fn expand_hex(value: char) -> Result<u8, ParseError> {
        let mut pair = String::with_capacity(2);
        pair.push(value);
        pair.push(value);
        Self::parse_hex_pair(&pair)
    }
}

#[cfg(test)]
mod tests {
    use super::{Color, Selector, Unit, Value, parse};

    #[test]
    fn parses_multiple_rules_and_selectors() {
        let stylesheet = parse(
            r#"
                h1, .title {
                    color: #ff0000;
                    font-size: 24px;
                }

                #app {
                    display: block;
                }
            "#,
        )
        .unwrap();

        assert_eq!(stylesheet.rules.len(), 2);
        assert_eq!(
            stylesheet.rules[0].selectors,
            vec![Selector::Tag("h1".into()), Selector::Class("title".into())]
        );
        assert_eq!(
            stylesheet.rules[0].declarations[0].value,
            Value::Color(Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            })
        );
        assert_eq!(
            stylesheet.rules[0].declarations[1].value,
            Value::Length(24.0, Unit::Px)
        );
        assert_eq!(
            stylesheet.rules[1].selectors,
            vec![Selector::Id("app".into())]
        );
    }

    #[test]
    fn parses_keyword_and_shorthand_hex_values() {
        let stylesheet = parse(
            r#"
                p {
                    display: block;
                    color: #0f0;
                }
            "#,
        )
        .unwrap();

        let declarations = &stylesheet.rules[0].declarations;
        assert_eq!(declarations[0].value, Value::Keyword("block".into()));
        assert_eq!(
            declarations[1].value,
            Value::Color(Color {
                r: 0,
                g: 255,
                b: 0,
                a: 255,
            })
        );
    }

    #[test]
    fn border_radius_single_value_expands_to_uniform_corners() {
        let stylesheet = parse("div { border-radius: 8px; }").unwrap();
        let names: Vec<&str> = stylesheet.rules[0]
            .declarations
            .iter()
            .map(|decl| decl.name.as_str())
            .collect();

        assert_eq!(
            names,
            vec![
                "border-top-left-radius",
                "border-top-right-radius",
                "border-bottom-right-radius",
                "border-bottom-left-radius",
            ]
        );
        for decl in &stylesheet.rules[0].declarations {
            assert_eq!(decl.value, Value::Length(8.0, Unit::Px));
        }
    }

    #[test]
    fn border_radius_two_values_pair_diagonal_corners() {
        let stylesheet = parse("div { border-radius: 8px 12px; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;

        // 2-value shorthand: first is tl/br, second is tr/bl.
        assert_eq!(decls[0].name, "border-top-left-radius");
        assert_eq!(decls[0].value, Value::Length(8.0, Unit::Px));
        assert_eq!(decls[1].name, "border-top-right-radius");
        assert_eq!(decls[1].value, Value::Length(12.0, Unit::Px));
        assert_eq!(decls[2].name, "border-bottom-right-radius");
        assert_eq!(decls[2].value, Value::Length(8.0, Unit::Px));
        assert_eq!(decls[3].name, "border-bottom-left-radius");
        assert_eq!(decls[3].value, Value::Length(12.0, Unit::Px));
    }

    #[test]
    fn border_radius_four_values_assign_each_corner() {
        let stylesheet = parse("div { border-radius: 1px 2px 3px 4px; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;

        assert_eq!(decls[0].value, Value::Length(1.0, Unit::Px));
        assert_eq!(decls[1].value, Value::Length(2.0, Unit::Px));
        assert_eq!(decls[2].value, Value::Length(3.0, Unit::Px));
        assert_eq!(decls[3].value, Value::Length(4.0, Unit::Px));
    }

    #[test]
    fn border_radius_three_values_share_minor_diagonal() {
        let stylesheet = parse("div { border-radius: 1px 2px 3px; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;

        // 3-value shorthand: tl, tr/bl, br.
        assert_eq!(decls[0].value, Value::Length(1.0, Unit::Px));
        assert_eq!(decls[1].value, Value::Length(2.0, Unit::Px));
        assert_eq!(decls[2].value, Value::Length(3.0, Unit::Px));
        assert_eq!(decls[3].value, Value::Length(2.0, Unit::Px));
    }

    #[test]
    fn explicit_corner_property_overrides_shorthand_when_listed_after() {
        let stylesheet =
            parse("div { border-radius: 8px; border-top-left-radius: 12px; }").unwrap();
        let decls = &stylesheet.rules[0].declarations;

        // Shorthand still expands first; the explicit property follows and wins on cascade.
        assert_eq!(decls.len(), 5);
        assert_eq!(decls[4].name, "border-top-left-radius");
        assert_eq!(decls[4].value, Value::Length(12.0, Unit::Px));
    }

    #[test]
    fn skips_invalid_declarations() {
        let stylesheet = parse("div { color red; font-size: 16px; }").unwrap();

        // The malformed "color red" declaration is skipped; valid ones are kept.
        assert_eq!(stylesheet.rules.len(), 1);
        assert_eq!(stylesheet.rules[0].declarations.len(), 1);
        assert_eq!(stylesheet.rules[0].declarations[0].name, "font-size");
    }
}
