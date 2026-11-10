// cssparser → `ParseError` conversion helpers. These translate the upstream
// `ParseError` / `BasicParseError` variants into our flat `ParseError`
// (single position + human message). Pulled out of `css/mod.rs` so the
// per-grammar parsers in sibling files can depend on them directly.

use cssparser::{BasicParseErrorKind, ParseError as CssParseError, Parser as CssParser, Token};

use super::{CssError, ParseError};

// cssparser 0.38 dropped `location` from `ParseError` and the token / at-rule
// name payloads from `BasicParseErrorKind` (to keep `Result<&Token, _>` in
// registers), so basic errors no longer carry a position — report 0.
pub(super) fn convert_error(err: CssParseError<CssError>) -> ParseError {
    let position = 0;
    match err.kind {
        cssparser::ParseErrorKind::Custom(custom) => custom,
        cssparser::ParseErrorKind::Basic(basic) => match basic {
            BasicParseErrorKind::EndOfInput => ParseError::new(position, "unexpected end of input"),
            BasicParseErrorKind::UnexpectedToken => ParseError::new(position, "unexpected token"),
            BasicParseErrorKind::AtRuleInvalid => ParseError::new(position, "invalid at-rule"),
            BasicParseErrorKind::AtRuleBodyInvalid => {
                ParseError::new(position, "invalid at-rule body")
            }
            BasicParseErrorKind::QualifiedRuleInvalid => {
                ParseError::new(position, "invalid qualified rule")
            }
            BasicParseErrorKind::TooManyNestedBlocks => {
                ParseError::new(position, "nesting block limit reached")
            }
        },
    }
}

pub(super) fn convert_basic_error_at(
    position: usize,
    err: cssparser::BasicParseError,
) -> ParseError {
    let message = match err.kind {
        BasicParseErrorKind::EndOfInput => "unexpected end of input".to_string(),
        BasicParseErrorKind::UnexpectedToken => "unexpected token".to_string(),
        BasicParseErrorKind::AtRuleInvalid => "invalid at-rule".to_string(),
        BasicParseErrorKind::AtRuleBodyInvalid => "invalid at-rule body".to_string(),
        BasicParseErrorKind::QualifiedRuleInvalid => "invalid qualified rule".to_string(),
        BasicParseErrorKind::TooManyNestedBlocks => "nesting block limit reached".to_string(),
    };
    ParseError::new(position, message)
}

pub(super) fn token_error<'i>(
    input: &CssParser<'i>,
    token: &Token<'_>,
    message: &str,
) -> ParseError {
    ParseError::new(
        input.position().byte_index(),
        format!("{message}: got {token:?}"),
    )
}
