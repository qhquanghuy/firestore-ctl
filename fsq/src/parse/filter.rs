//! `--where` filters: `<field> <op> <value>`.

use std::str::FromStr;

use super::{ParseError, parse_token, unquoted};
use crate::domain::{FieldPath, Filter, Op, Value};

impl FromStr for Filter {
    type Err = ParseError;

    /// Splits at the first whitespace-delimited word that is an operator; the field
    /// is everything before it (backtick-quote fields containing spaces) and the
    /// value, parsed as a [token](parse_token), is the rest. A list value is
    /// required for `in`, `not-in` and `array-contains-any`, and rejected otherwise.
    fn from_str(s: &str) -> Result<Self, ParseError> {
        let trimmed = s.trim();
        let (field, op, value) = split(trimmed).ok_or_else(|| {
            ParseError::new(
                s,
                "expected `<field> <op> <value>` with spaces around the operator",
            )
        })?;
        let field: FieldPath = field
            .parse()
            .map_err(|e| ParseError::new(s, format!("{e}")))?;
        let value = parse_token(value).map_err(|e| ParseError::new(s, e.to_string()))?;
        match (op.takes_list(), matches!(value, Value::Array(_))) {
            (true, false) => Err(ParseError::new(
                s,
                format!("`{}` needs a list value like [a, b]", op.symbol()),
            )),
            (false, true) => Err(ParseError::new(
                s,
                format!(
                    "`{}` does not take a list (quote it to match a string)",
                    op.symbol()
                ),
            )),
            _ => Ok(Filter { field, op, value }),
        }
    }
}

/// `(field, operator, value)` around the first operator word, if there is one.
fn split(s: &str) -> Option<(&str, Op, &str)> {
    unquoted(s, '`')
        .filter(|&(_, c)| c.is_whitespace())
        .find_map(|(at, _)| {
            let (word, value) = s[at..].trim_start().split_once(char::is_whitespace)?;
            Some((&s[..at], Op::from_symbol(word)?, value.trim()))
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn filters_with_spaces_and_quotes() {
        let f: Filter = r#"name == "John Smith""#.parse().unwrap();
        assert_eq!(
            (f.op, f.value),
            (Op::Eq, Value::String("John Smith".into()))
        );
        let g: Filter = r#"note == "a == b""#.parse().unwrap();
        assert_eq!(g.value, Value::String("a == b".into()));
        let h: Filter = r#"id in [a, "b c", 3]"#.parse().unwrap();
        assert_eq!(
            h.value,
            Value::Array(vec!["a".into(), "b c".into(), Value::Int(3)])
        );
        assert_eq!(
            "tags array-contains vip".parse::<Filter>().map(|f| f.op),
            Ok(Op::ArrayContains)
        );
        assert!("status active".parse::<Filter>().is_err());
        assert!("id in a".parse::<Filter>().is_err()); // `in` needs a list
    }

    #[test]
    fn every_operator_parses() {
        for (text, op) in [
            ("==", Op::Eq),
            ("!=", Op::Ne),
            ("<", Op::Lt),
            ("<=", Op::Le),
            (">", Op::Gt),
            (">=", Op::Ge),
            ("not-in", Op::NotIn),
            ("array-contains-any", Op::ArrayContainsAny),
        ] {
            let value = if op.takes_list() { "[1, 2]" } else { "1" };
            let f: Filter = format!("age {text} {value}").parse().unwrap();
            assert_eq!(f.op, op, "{text}");
        }
    }

    #[test]
    fn operators_need_whitespace_and_a_value() {
        assert!("age>=18".parse::<Filter>().is_err());
        assert!("age ==".parse::<Filter>().is_err());
        assert!("== 3".parse::<Filter>().is_err());
        assert!("".parse::<Filter>().is_err());
    }

    #[test]
    fn fields_may_be_quoted_or_nested() {
        let f: Filter = "`my field` == 1".parse().unwrap();
        assert_eq!(f.field.segments(), ["my field"]);
        let g: Filter = "address.city   ==   Tokyo".parse().unwrap();
        assert_eq!(g.field.segments(), ["address", "city"]);
        assert_eq!(g.value, Value::String("Tokyo".into()));
        let h: Filter = "`a == b` == 1".parse().unwrap();
        assert_eq!(h.field.segments(), ["a == b"]);
    }

    #[test]
    fn lists_only_where_an_array_is_expected() {
        assert!("age == [1, 2]".parse::<Filter>().is_err());
        assert!("tags array-contains [a]".parse::<Filter>().is_err());
        assert!("age < [1]".parse::<Filter>().is_err());
        assert_eq!(
            "tags array-contains-any [a, b]"
                .parse::<Filter>()
                .map(|f| f.value),
            Ok(Value::Array(vec!["a".into(), "b".into()]))
        );
    }

    #[test]
    fn errors_show_the_whole_input() {
        let e = "status active".parse::<Filter>().unwrap_err();
        assert_eq!(e.input, "status active");
        assert!(e.to_string().starts_with("status active: "), "{e}");
    }
}
