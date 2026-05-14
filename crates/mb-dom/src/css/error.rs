// cssparser → `ParseError` conversion helpers. These translate the upstream
// `ParseError` / `BasicParseError` variants into our flat `ParseError`
// (single position + human message). Pulled out of `css/mod.rs` so the
// per-grammar parsers in sibling files can depend on them directly.

use cssparser::{BasicParseErrorKind, Parser as CssParser, ParseError as CssParseError, Token};

use super::{CssError, ParseError};

pub(super) fn convert_error<'i>(err: CssParseError<'i, CssError>) -> ParseError {
    let position = err.location.line as usize; // best-effort fallback
    match err.kind {
        cssparser::ParseErrorKind::Custom(custom) => custom,
        cssparser::ParseErrorKind::Basic(basic) => match basic {
            BasicParseErrorKind::EndOfInput => ParseError::new(position, "unexpected end of input"),
            BasicParseErrorKind::UnexpectedToken(token) => {
                ParseError::new(position, format!("unexpected token: {token:?}"))
            }
            BasicParseErrorKind::AtRuleInvalid(name) => {
                ParseError::new(position, format!("invalid at-rule '@{name}'"))
            }
            BasicParseErrorKind::AtRuleBodyInvalid => {
                ParseError::new(position, "invalid at-rule body")
            }
            BasicParseErrorKind::QualifiedRuleInvalid => {
                ParseError::new(position, "invalid qualified rule")
            }
        },
    }
}

pub(super) fn convert_basic_error_at<'i>(
    position: usize,
    err: cssparser::BasicParseError<'i>,
) -> ParseError {
    let message = match err.kind {
        BasicParseErrorKind::EndOfInput => "unexpected end of input".to_string(),
        BasicParseErrorKind::UnexpectedToken(token) => format!("unexpected token: {token:?}"),
        BasicParseErrorKind::AtRuleInvalid(name) => format!("invalid at-rule '@{name}'"),
        BasicParseErrorKind::AtRuleBodyInvalid => "invalid at-rule body".to_string(),
        BasicParseErrorKind::QualifiedRuleInvalid => "invalid qualified rule".to_string(),
    };
    ParseError::new(position, message)
}

pub(super) fn token_error<'i, 't>(
    input: &CssParser<'i, 't>,
    token: &Token<'_>,
    message: &str,
) -> ParseError {
    ParseError::new(
        input.position().byte_index(),
        format!("{message}: got {token:?}"),
    )
}
