//! Parsing of command-line flag syntax into domain values:
//! typed value tokens, `--where` filters, `--order-by` terms and `--set` field ops.

use thiserror::Error;

use crate::domain::{FieldPath, FieldPathError};

pub mod field_op;
pub mod filter;
pub mod order;
pub mod token;

pub use token::parse_token;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{input}: {reason}")]
pub struct ParseError {
    pub input: String,
    pub reason: String,
}

impl ParseError {
    pub(crate) fn new(input: &str, reason: impl Into<String>) -> Self {
        Self {
            input: input.to_owned(),
            reason: reason.into(),
        }
    }
}

/// Yields `(byte index, char)` for the characters of `s` outside `quote`-delimited
/// segments. The delimiters and everything between them are skipped; inside a
/// segment a backslash escapes the next character, so `\<quote>` does not close it.
pub(crate) fn unquoted(s: &str, quote: char) -> impl Iterator<Item = (usize, char)> + '_ {
    s.char_indices()
        .scan((false, false), move |(in_quote, escaped), (i, c)| {
            let visible = match (*in_quote, *escaped, c) {
                (true, true, _) => {
                    *escaped = false;
                    false
                }
                (true, false, '\\') => {
                    *escaped = true;
                    false
                }
                (true, false, c) if c == quote => {
                    *in_quote = false;
                    false
                }
                (true, false, _) => false,
                (false, _, c) if c == quote => {
                    *in_quote = true;
                    false
                }
                (false, _, _) => true,
            };
            Some(visible.then_some((i, c)))
        })
        .flatten()
}

/// Characters that may not appear in an unquoted field name in a flag: they would
/// make operators, directions or typos parse as part of the name.
const RESERVED_IN_FIELD: &str = "<>!=+-:";

/// Parses a field path from a flag. `hint` is appended to the rejection error to point at
/// the likely intended syntax.
pub(crate) fn parse_field(text: &str, hint: &str) -> Result<FieldPath, String> {
    let text = text.trim();
    match unquoted(text, '`').find(|&(_, c)| c.is_whitespace() || RESERVED_IN_FIELD.contains(c)) {
        Some((_, c)) => Err(format!(
            "unquoted field name `{}` contains {}; {hint}",
            text,
            if c.is_whitespace() {
                "whitespace".to_owned()
            } else {
                format!("`{c}`")
            }
        )),
        None => text.parse().map_err(|e: FieldPathError| e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unquoted_skips_quoted_segments_and_escapes() {
        let visible = |s: &str, q| unquoted(s, q).map(|(_, c)| c).collect::<String>();
        assert_eq!(visible("a`b=c`=d", '`'), "a=d");
        assert_eq!(visible(r"`a\`=`=", '`'), "=");
        assert_eq!(visible(r#"x "a, b" ,y"#, '"'), "x  ,y");
        assert_eq!(visible("`open=", '`'), "");
    }

    #[test]
    fn parse_field_trims_but_rejects_inner_whitespace_and_punctuation() {
        assert_eq!(
            parse_field(" a.b ", "h").map(|p| p.to_string()),
            Ok("a.b".into())
        );
        assert!(parse_field("`a b`.c", "h").is_ok());
        for bad in ["a b", "a-b", "a:b", "a=b", "a>b", "a!b", "a+b"] {
            let e = parse_field(bad, "HINT").unwrap_err();
            assert!(e.ends_with("HINT"), "{bad}: {e}");
        }
    }
}
