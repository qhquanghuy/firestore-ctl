use super::{ParseError, split_unquoted};
use crate::domain::{DocPath, Timestamp, Value};

/// Anything not recognised as a typed token is a string.
pub fn parse_token(s: &str) -> Result<Value, ParseError> {
    match s {
        "null" => Ok(Value::Null),
        "true" => Ok(Value::Bool(true)),
        "false" => Ok(Value::Bool(false)),
        _ => {
            if let Some(body) = s.strip_prefix('"') {
                quoted(s, body).map(Value::String)
            } else if let Some(text) = s.strip_prefix("ts:") {
                text.parse::<Timestamp>().map(Value::Timestamp).map_err(|e| {
                    ParseError::new(
                        s,
                        format!("invalid timestamp (expected RFC 3339, e.g. 2026-01-05T10:00:00Z): {e}"),
                    )
                })
            } else if let Some(text) = s.strip_prefix("ref:") {
                text.parse::<DocPath>()
                    .map(Value::Ref)
                    .map_err(|e| ParseError::new(s, format!("invalid document reference: {e}")))
            } else if let Some(inner) = s.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                list(s, inner).map(Value::Array)
            } else {
                number(s).unwrap_or_else(|| Ok(Value::String(s.to_owned())))
            }
        }
    }
}

fn quoted(token: &str, body: &str) -> Result<String, ParseError> {
    let mut out = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' if chars.as_str().is_empty() => return Ok(out),
            '"' => {
                return Err(ParseError::new(
                    token,
                    "unexpected text after the closing quote",
                ));
            }
            '\\' => match chars.next() {
                Some(escaped @ ('"' | '\\')) => out.push(escaped),
                Some(other) => {
                    return Err(ParseError::new(
                        token,
                        format!("invalid escape `\\{other}`; only \\\" and \\\\ are allowed"),
                    ));
                }
                None => break,
            },
            _ => out.push(c),
        }
    }
    Err(ParseError::new(token, "unterminated quoted string"))
}

fn list(token: &str, inner: &str) -> Result<Vec<Value>, ParseError> {
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    split_unquoted(inner, '"', ',')
        .into_iter()
        .map(str::trim)
        .map(|element| match element {
            "" => Err(ParseError::new(token, "empty list element")),
            e if e.contains('"') && !e.starts_with('"') => Err(ParseError::new(
                token,
                "stray quote in list element (quote the whole element)",
            )),
            e if e.starts_with('[') => Err(ParseError::new(
                token,
                "nested lists are not supported (quote the element to make it a string)",
            )),
            e => parse_token(e).map_err(|inner| {
                ParseError::new(token, format!("list element `{e}`: {}", inner.reason))
            }),
        })
        .collect()
}

/// Only a canonical integer (no leading `+` or zeros, which would be lost) or a float is a number;
/// a number-shaped token that cannot be represented is an error, not a string.
fn number(s: &str) -> Option<Result<Value, ParseError>> {
    if s.starts_with('+') {
        return None;
    }
    let unsigned = s.strip_prefix('-').unwrap_or(s);
    if !unsigned.is_empty() && unsigned.chars().all(|c| c.is_ascii_digit()) {
        let canonical = unsigned == "0" || !unsigned.starts_with('0');
        return canonical.then(|| s.parse::<i64>().map(Value::Int).map_err(|_overflow| {
            ParseError::new(
                s,
                "integer does not fit in 64 bits (quote it to use a string, or write it as a float like 1e20)",
            )
        }));
    }
    let float_shaped = s.chars().any(|c| c.is_ascii_digit())
        && s.chars().any(|c| matches!(c, '.' | 'e' | 'E'))
        && s.chars().all(|c| c.is_ascii_digit() || "+-.eE".contains(c));
    float_shaped
        .then(|| s.parse::<f64>().ok())
        .flatten()
        .map(|f| {
            if f.is_finite() {
                Ok(Value::Double(f))
            } else {
                Err(ParseError::new(s, "float is out of range"))
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_tokens() {
        assert_eq!(parse_token("18"), Ok(Value::Int(18)));
        assert_eq!(parse_token("1.5"), Ok(Value::Double(1.5)));
        assert_eq!(parse_token("\"18\""), Ok(Value::String("18".into())));
        assert_eq!(parse_token("active"), Ok(Value::String("active".into())));
        assert!(matches!(
            parse_token("ts:2026-01-05T10:00:00Z"),
            Ok(Value::Timestamp(_))
        ));
        assert!(matches!(parse_token("ref:teams/t1"), Ok(Value::Ref(_))));
        assert!(parse_token("ref:teams").is_err());
        assert!(parse_token("ts:yesterday").is_err());
    }

    #[test]
    fn keywords_and_numbers() {
        assert_eq!(parse_token("null"), Ok(Value::Null));
        assert_eq!(parse_token("true"), Ok(Value::Bool(true)));
        assert_eq!(parse_token("false"), Ok(Value::Bool(false)));
        assert_eq!(parse_token("-7"), Ok(Value::Int(-7)));
        assert_eq!(parse_token("1e3"), Ok(Value::Double(1000.0)));
        assert_eq!(parse_token("-0.5"), Ok(Value::Double(-0.5)));
        assert_eq!(parse_token("0"), Ok(Value::Int(0)));
        assert_eq!(parse_token("-0"), Ok(Value::Int(0)));
        assert_eq!(parse_token(".5"), Ok(Value::Double(0.5)));
        assert_eq!(parse_token("5."), Ok(Value::Double(5.0)));
        assert_eq!(parse_token("2E-2"), Ok(Value::Double(0.02)));
        for s in [
            "+5",
            "+8190123456",
            "007",
            "01234",
            "-007",
            "NaN",
            "inf",
            "1.2.3",
            "2026-01-05",
            "e",
            "-",
            "1-2",
            "True",
            "",
        ] {
            assert_eq!(parse_token(s), Ok(Value::String(s.into())), "{s:?}");
        }
        assert!(parse_token("99999999999999999999").is_err());
        assert!(parse_token("1e999").is_err());
    }

    #[test]
    fn timestamps_keep_nanos() {
        let Ok(Value::Timestamp(t)) = parse_token("ts:2026-01-05T10:00:00.123456789+09:00") else {
            panic!("not a timestamp")
        };
        assert_eq!(t.to_string(), "2026-01-05T01:00:00.123456789Z");
    }

    #[test]
    fn list_element_errors_name_the_element_once() {
        let e = parse_token("[a, ts:soon]").unwrap_err();
        assert_eq!(e.input, "[a, ts:soon]");
        assert!(
            e.reason
                .starts_with("list element `ts:soon`: invalid timestamp"),
            "{e}"
        );
        assert!(!e.reason.contains("ts:soon: "), "{e}");
    }

    #[test]
    fn quoted_strings_and_escapes() {
        assert_eq!(parse_token(r#""""#), Ok(Value::String(String::new())));
        assert_eq!(
            parse_token(r#""a \"b\" \\c""#),
            Ok(Value::String(r#"a "b" \c"#.into()))
        );
        assert_eq!(parse_token(r#""null""#), Ok(Value::String("null".into())));
        assert_eq!(parse_token(r#""[a]""#), Ok(Value::String("[a]".into())));
        assert!(parse_token(r#""abc"#).is_err());
        assert!(parse_token(r#""abc" def"#).is_err());
        assert!(parse_token(r#""a\nb""#).is_err());
    }

    #[test]
    fn lists_are_typed_and_quote_aware() {
        assert_eq!(parse_token("[]"), Ok(Value::Array(vec![])));
        assert_eq!(parse_token("[ ]"), Ok(Value::Array(vec![])));
        let Ok(Value::Array(items)) =
            parse_token(r#"[a, "b, c" ,3,1.5, null,ts:2026-01-05T10:00:00Z]"#)
        else {
            panic!("not a list")
        };
        assert_eq!(items.len(), 6);
        assert_eq!(
            parse_token(r#"["x]", "y"]"#),
            Ok(Value::Array(vec!["x]".into(), "y".into()]))
        );
        assert!(parse_token("[a,]").is_err());
        assert!(parse_token("[a,,b]").is_err());
        assert!(parse_token(r#"[5", 6"]"#).is_err());
        assert!(parse_token("[[a], b]").is_err());
        assert!(parse_token(r#"[a, "b]"#).is_err());
        assert_eq!(
            parse_token("[draft] foo"),
            Ok(Value::String("[draft] foo".into()))
        );
        assert_eq!(parse_token("[a"), Ok(Value::String("[a".into())));
    }
}
