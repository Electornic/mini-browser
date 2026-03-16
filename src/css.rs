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
    let stylesheet = parser.parse_stylesheet()?;
    parser.consume_whitespace();

    if parser.eof() {
        Ok(stylesheet)
    } else {
        Err(ParseError::new(
            parser.pos,
            "unexpected trailing input after parsing stylesheet",
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

    fn parse_stylesheet(&mut self) -> Result<Stylesheet, ParseError> {
        let mut rules = Vec::new();

        loop {
            self.consume_whitespace();

            if self.eof() {
                break;
            }

            rules.push(self.parse_rule()?);
        }

        Ok(Stylesheet { rules })
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
            self.consume_whitespace();
            selectors.push(self.parse_selector()?);
            self.consume_whitespace();

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
            self.consume_whitespace();

            if self.eof() || self.next_char() == Some('}') {
                break;
            }

            declarations.push(self.parse_declaration()?);
            self.consume_whitespace();

            if self.next_char() == Some(';') {
                self.consume_char();
            }
        }

        Ok(declarations)
    }

    fn parse_declaration(&mut self) -> Result<Declaration, ParseError> {
        let name = self.parse_identifier()?;
        self.consume_whitespace();
        self.expect_char(':')?;
        self.consume_whitespace();
        let value = self.parse_value()?;

        Ok(Declaration { name, value })
    }

    fn parse_value(&mut self) -> Result<Value, ParseError> {
        match self.next_char() {
            Some('#') => self.parse_hex_color(),
            Some(ch) if ch.is_ascii_digit() => self.parse_length_or_number(),
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
    fn returns_error_for_missing_colon() {
        let error = parse("div { color red; }").unwrap_err();

        assert!(error.message.contains("expected ':'"));
    }
}
