//! Field paths (`address.city`, `` `my-field`.x ``) and their Firestore rendering.

use std::fmt;
use std::str::FromStr;

use itertools::Itertools;
use thiserror::Error;

/// Why a string is not a valid field path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Error)]
pub enum FieldPathError {
    /// The path had no segments.
    #[error("field path must not be empty")]
    Empty,
    /// The segment at this zero-based position was empty (e.g. `a..b`).
    #[error("field path `{input}` has an empty segment at position {position}")]
    EmptySegment { input: String, position: usize },
    /// A backtick-quoted segment was never closed.
    #[error("field path `{0}` has an unterminated backtick quote")]
    UnterminatedQuote(String),
    /// A backtick appeared inside an unquoted segment, or text followed a closing quote.
    #[error("field path `{0}` has an unexpected backtick or text after a quoted segment")]
    Unexpected(String),
}

/// A path to a (possibly nested) field: a non-empty list of segments.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FieldPath(Vec<String>);

impl FieldPath {
    /// The unquoted, unescaped segments, outermost first.
    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.0
    }

    /// Renders for Firestore: segments that are not plain identifiers
    /// (`[A-Za-z_][A-Za-z0-9_]*`) are backtick-quoted, escaping `` ` `` and `\`.
    #[must_use]
    pub fn to_firestore(&self) -> String {
        self.0.iter().map(|s| quote_if_needed(s)).join(".")
    }
}

impl fmt::Display for FieldPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_firestore())
    }
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn quote_if_needed(segment: &str) -> String {
    if is_identifier(segment) {
        segment.to_owned()
    } else {
        let escaped = segment.replace('\\', "\\\\").replace('`', "\\`");
        format!("`{escaped}`")
    }
}

/// Reads one segment off the front of `rest`; returns it with the text after it.
fn take_segment<'a>(rest: &'a str, input: &str) -> Result<(String, &'a str), FieldPathError> {
    if let Some(quoted) = rest.strip_prefix('`') {
        return take_quoted(quoted, input);
    }
    let end = rest.find(['.', '`']).unwrap_or(rest.len());
    if rest[end..].starts_with('`') {
        Err(FieldPathError::Unexpected(input.to_owned()))
    } else {
        Ok((rest[..end].to_owned(), &rest[end..]))
    }
}

/// Reads a quoted segment body (after the opening backtick) up to the closing backtick.
fn take_quoted<'a>(body: &'a str, input: &str) -> Result<(String, &'a str), FieldPathError> {
    let mut out = String::new();
    let mut chars = body.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '`' => return Ok((out, &body[i + 1..])),
            '\\' => match chars.next() {
                Some((_, escaped)) => out.push(escaped),
                None => break,
            },
            _ => out.push(c),
        }
    }
    Err(FieldPathError::UnterminatedQuote(input.to_owned()))
}

/// Reads segments from `rest` (which starts at a segment), accumulating into `done`.
fn read_segments(
    rest: &str,
    input: &str,
    done: Vec<String>,
) -> Result<Vec<String>, FieldPathError> {
    let (segment, after) = take_segment(rest, input)?;
    if segment.is_empty() {
        return Err(FieldPathError::EmptySegment {
            input: input.to_owned(),
            position: done.len(),
        });
    }
    let done = done.into_iter().chain(Some(segment)).collect();
    match after.strip_prefix('.') {
        Some(next) => read_segments(next, input, done),
        None if after.is_empty() => Ok(done),
        None => Err(FieldPathError::Unexpected(input.to_owned())),
    }
}

impl FromStr for FieldPath {
    type Err = FieldPathError;

    /// Dot-separated segments; wrap a segment in backticks to include dots or
    /// other punctuation, escaping `` ` `` and `\` with a backslash.
    fn from_str(s: &str) -> Result<Self, FieldPathError> {
        if s.is_empty() {
            return Err(FieldPathError::Empty);
        }
        read_segments(s, s, Vec::new()).map(Self)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn field_path_quotes_non_identifier_segments() {
        let f = |s: &str| s.parse::<FieldPath>().map(|p| p.to_firestore());
        assert_eq!(f("address.city"), Ok("address.city".into()));
        assert_eq!(f("`my-field`.x"), Ok("`my-field`.x".into()));
        assert_eq!(f("my-field"), Ok("`my-field`".into()));
        assert_eq!(f("1st"), Ok("`1st`".into()));
        assert_eq!(f("with space"), Ok("`with space`".into()));
        assert_eq!(f("`a\\`b`"), Ok("`a\\`b`".into()));
    }

    #[test]
    fn segments_are_unquoted_and_unescaped() {
        let p: FieldPath = "`a.b`.c.`x\\`y`".parse().unwrap();
        assert_eq!(p.segments(), ["a.b", "c", "x`y"]);
        let backslash: FieldPath = "`a\\\\b`".parse().unwrap();
        assert_eq!(backslash.segments(), ["a\\b"]);
        assert_eq!(backslash.to_firestore(), "`a\\\\b`");
    }

    #[test]
    fn display_round_trips() {
        let p: FieldPath = "`a.b`.c-d.e".parse().unwrap();
        assert_eq!(p.to_string().parse::<FieldPath>(), Ok(p));
    }

    #[test]
    fn rejects_malformed_field_paths() {
        assert_eq!("".parse::<FieldPath>(), Err(FieldPathError::Empty));
        assert!(matches!(
            "a..b".parse::<FieldPath>(),
            Err(FieldPathError::EmptySegment { position: 1, .. })
        ));
        assert!(matches!(
            "a.".parse::<FieldPath>(),
            Err(FieldPathError::EmptySegment { position: 1, .. })
        ));
        assert!(matches!(
            "``".parse::<FieldPath>(),
            Err(FieldPathError::EmptySegment { position: 0, .. })
        ));
        assert!(matches!(
            "`abc".parse::<FieldPath>(),
            Err(FieldPathError::UnterminatedQuote(_))
        ));
        assert!(matches!(
            "a`b".parse::<FieldPath>(),
            Err(FieldPathError::Unexpected(_))
        ));
        assert!(matches!(
            "`a`b".parse::<FieldPath>(),
            Err(FieldPathError::Unexpected(_))
        ));
    }
}
