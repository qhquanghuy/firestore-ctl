use std::fmt;
use std::str::FromStr;

use itertools::Itertools;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Error)]
pub enum FieldPathError {
    #[error("field path must not be empty")]
    Empty,
    #[error("field path `{input}` has an empty segment at position {position}")]
    EmptySegment { input: String, position: usize },
    #[error("field path `{0}` has an unterminated backtick quote")]
    UnterminatedQuote(String),
    #[error("field path `{0}` has an unexpected backtick or text after a quoted segment")]
    Unexpected(String),
    #[error(
        "field path `{input}` has an invalid escape `\\{escape}`; only \\` and \\\\ are allowed"
    )]
    InvalidEscape { input: String, escape: char },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FieldPath(Vec<String>);

impl FieldPath {
    pub fn from_segments(
        segments: impl IntoIterator<Item = String>,
    ) -> Result<Self, FieldPathError> {
        let segments: Vec<String> = segments.into_iter().collect();
        non_empty_segments(segments, |segs| segs.iter().join("."))
    }

    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.0
    }

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

fn non_empty_segments(
    segments: Vec<String>,
    describe: impl FnOnce(&[String]) -> String,
) -> Result<FieldPath, FieldPathError> {
    if segments.is_empty() {
        return Err(FieldPathError::Empty);
    }
    match segments.iter().position(String::is_empty) {
        Some(position) => Err(FieldPathError::EmptySegment {
            input: describe(&segments),
            position,
        }),
        None => Ok(FieldPath(segments)),
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

fn take_quoted<'a>(body: &'a str, input: &str) -> Result<(String, &'a str), FieldPathError> {
    let mut out = String::new();
    let mut chars = body.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '`' => return Ok((out, &body[i + 1..])),
            '\\' => match chars.next() {
                Some((_, escaped @ ('`' | '\\'))) => out.push(escaped),
                Some((_, escape)) => {
                    return Err(FieldPathError::InvalidEscape {
                        input: input.to_owned(),
                        escape,
                    });
                }
                None => break,
            },
            _ => out.push(c),
        }
    }
    Err(FieldPathError::UnterminatedQuote(input.to_owned()))
}

impl FromStr for FieldPath {
    type Err = FieldPathError;

    fn from_str(s: &str) -> Result<Self, FieldPathError> {
        if s.is_empty() {
            return Err(FieldPathError::Empty);
        }
        let mut segments = Vec::new();
        let mut rest = s;
        loop {
            let (segment, after) = take_segment(rest, s)?;
            segments.push(segment);
            match after.strip_prefix('.') {
                Some(next) => rest = next,
                None if after.is_empty() => break,
                None => return Err(FieldPathError::Unexpected(s.to_owned())),
            }
        }
        non_empty_segments(segments, |_| s.to_owned())
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

    #[test]
    fn only_backtick_and_backslash_escapes_are_allowed() {
        assert!(matches!(
            "`a\\nb`".parse::<FieldPath>(),
            Err(FieldPathError::InvalidEscape { escape: 'n', .. })
        ));
        assert!(matches!(
            "`a\\.b`".parse::<FieldPath>(),
            Err(FieldPathError::InvalidEscape { escape: '.', .. })
        ));
        let msg = "`a\\nb`".parse::<FieldPath>().unwrap_err().to_string();
        assert!(msg.contains("a\\nb"), "{msg}");
    }

    #[test]
    fn very_long_paths_parse_without_deep_recursion() {
        let long = vec!["a"; 200_000].join(".");
        assert_eq!(long.parse::<FieldPath>().unwrap().segments().len(), 200_000);
    }

    #[test]
    fn from_segments_keeps_raw_segments() {
        let p = FieldPath::from_segments(["a.b".to_owned(), "c d".to_owned()]).unwrap();
        assert_eq!(p.segments(), ["a.b", "c d"]);
        assert_eq!(p.to_firestore(), "`a.b`.`c d`");
        assert_eq!(p.to_string().parse::<FieldPath>(), Ok(p));
    }

    #[test]
    fn from_segments_rejects_empty_input() {
        assert_eq!(
            FieldPath::from_segments(Vec::<String>::new()),
            Err(FieldPathError::Empty)
        );
        assert!(matches!(
            FieldPath::from_segments(["a".to_owned(), String::new()]),
            Err(FieldPathError::EmptySegment { position: 1, .. })
        ));
    }
}
