//! Parsing of command-line flag syntax into domain values:
//! typed value tokens, `--where` filters, `--order-by` terms and `--set` field ops.

use thiserror::Error;

pub mod field_op;
pub mod filter;
pub mod order;
pub mod token;

pub use token::parse_token;

/// A flag value that could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{input}: {reason}")]
pub struct ParseError {
    /// The text that was rejected.
    pub input: String,
    /// What is wrong with it.
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
}
