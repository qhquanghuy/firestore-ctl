//! `--set` field changes: `field=value`, `field+=value|[..]`, `field-=value|[..]`.

use std::str::FromStr;

use super::{ParseError, parse_token, unquoted};
use crate::domain::{FieldOp, FieldPath, Value};

impl FromStr for FieldOp {
    type Err = ParseError;

    /// Splits at the first `=` outside a backtick-quoted field; a `+` or `-` right
    /// before it makes it an array union or removal. The value, trimmed, is parsed
    /// as a [token](parse_token); `+=`/`-=` take a list or a single element.
    fn from_str(s: &str) -> Result<Self, ParseError> {
        let (eq, _) = unquoted(s, '`').find(|&(_, c)| c == '=').ok_or_else(|| {
            ParseError::new(
                s,
                "expected `field=value`, `field+=value` or `field-=value`",
            )
        })?;
        let field = |text: &str| {
            text.trim()
                .parse::<FieldPath>()
                .map_err(|e| ParseError::new(s, e.to_string()))
        };
        let value =
            parse_token(s[eq + 1..].trim()).map_err(|e| ParseError::new(s, e.to_string()))?;
        let before = &s[..eq];
        if let Some(text) = before.strip_suffix('+') {
            Ok(Self::ArrayUnion(field(text)?, elements(value)))
        } else if let Some(text) = before.strip_suffix('-') {
            Ok(Self::ArrayRemove(field(text)?, elements(value)))
        } else {
            Ok(Self::Set(field(before)?, value))
        }
    }
}

/// A list's elements, or a lone value as a one-element list.
fn elements(value: Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items,
        single => vec![single],
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::domain::Value;

    #[test]
    fn field_ops() {
        assert!(matches!(
            "plan=pro".parse(),
            Ok(FieldOp::Set(_, Value::String(_)))
        ));
        assert!(matches!("tags+=vip".parse(), Ok(FieldOp::ArrayUnion(_, v)) if v.len() == 1));
        assert!(matches!("tags-=[a, b]".parse(), Ok(FieldOp::ArrayRemove(_, v)) if v.len() == 2));
        assert!(matches!("note=a=b".parse(), Ok(FieldOp::Set(_, Value::String(s))) if s == "a=b"));
    }

    #[test]
    fn set_takes_typed_values_including_lists() {
        assert!(matches!(
            "age=18".parse(),
            Ok(FieldOp::Set(_, Value::Int(18)))
        ));
        assert!(
            matches!("tags=[a, b]".parse(), Ok(FieldOp::Set(_, Value::Array(v))) if v.len() == 2)
        );
        assert!(matches!("note=".parse(), Ok(FieldOp::Set(_, Value::String(s))) if s.is_empty()));
    }

    #[test]
    fn only_the_first_equals_decides_the_operator() {
        assert!(
            matches!("note=a+=b".parse(), Ok(FieldOp::Set(_, Value::String(s))) if s == "a+=b")
        );
        assert!(
            matches!("note=x-=y".parse(), Ok(FieldOp::Set(_, Value::String(s))) if s == "x-=y")
        );
    }

    #[test]
    fn spaces_around_the_operator_are_ignored() {
        let op: FieldOp = "plan = pro".parse().unwrap();
        assert!(
            matches!(op, FieldOp::Set(p, Value::String(s)) if p.segments() == ["plan"] && s == "pro")
        );
        let op: FieldOp = "tags += vip".parse().unwrap();
        assert!(matches!(op, FieldOp::ArrayUnion(p, _) if p.segments() == ["tags"]));
    }

    #[test]
    fn dashes_in_field_names_are_not_remove() {
        let op: FieldOp = "my-field=3".parse().unwrap();
        assert!(matches!(op, FieldOp::Set(p, Value::Int(3)) if p.segments() == ["my-field"]));
    }

    #[test]
    fn quoted_fields_hide_operators() {
        let op: FieldOp = "`a=b`=1".parse().unwrap();
        assert!(matches!(op, FieldOp::Set(p, Value::Int(1)) if p.segments() == ["a=b"]));
        let op: FieldOp = "`a+`=1".parse().unwrap();
        assert!(matches!(op, FieldOp::Set(p, _) if p.segments() == ["a+"]));
    }

    #[test]
    fn rejects_malformed_ops() {
        assert!("plan".parse::<FieldOp>().is_err());
        assert!("=x".parse::<FieldOp>().is_err());
        assert!("+=x".parse::<FieldOp>().is_err());
        assert!("a..b=1".parse::<FieldOp>().is_err());
        assert!("when=ts:soon".parse::<FieldOp>().is_err());
    }

    #[test]
    fn union_of_a_quoted_string_is_one_element() {
        assert!(
            matches!("tags+=\"[a]\"".parse(), Ok(FieldOp::ArrayUnion(_, v)) if v == [Value::from("[a]")])
        );
    }
}
