use std::str::FromStr;

use super::{ParseError, parse_field, parse_token, split_around, unquoted};
use crate::domain::{Filter, Op};

impl FromStr for Filter {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, ParseError> {
        let trimmed = s.trim();
        let (field, op, value) = split(trimmed).ok_or_else(|| {
            ParseError::new(
                s,
                "expected `<field> <op> <value>` with spaces around the operator",
            )
        })?;
        let field = parse_field(
            field,
            "backtick-quote field names with spaces or punctuation, e.g. `my-field`",
        )
        .map_err(|reason| ParseError::new(s, reason))?;
        let value = parse_token(value).map_err(|e| ParseError::new(s, e.reason))?;
        Self::new(field, op, value).map_err(|e| ParseError::new(s, e.to_string()))
    }
}

fn split(s: &str) -> Option<(&str, Op, &str)> {
    unquoted(s, '`')
        .filter(|&(_, c)| c.is_whitespace())
        .find_map(|(at, _)| {
            let (field, _, rest) = split_around(s, at)?;
            let (word, value) = rest.trim_start().split_once(char::is_whitespace)?;
            Some((field, Op::from_symbol(word)?, value.trim()))
        })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::domain::Value;
    use crate::domain::arb;

    fn parts(s: &str) -> (String, Op, Value) {
        let f: Filter = s.parse().unwrap();
        (f.field().to_string(), f.op(), f.value().clone())
    }

    #[test]
    fn filters_with_spaces_and_quotes() {
        let f: Filter = r#"name == "John Smith""#.parse().unwrap();
        assert_eq!(
            (f.op(), f.value().clone()),
            (Op::Eq, Value::String("John Smith".into()))
        );
        let g: Filter = r#"note == "a == b""#.parse().unwrap();
        assert_eq!(g.value(), &Value::String("a == b".into()));
        let h: Filter = r#"id in [a, "b c", 3]"#.parse().unwrap();
        assert_eq!(
            h.value(),
            &Value::Array(vec!["a".into(), "b c".into(), Value::Int(3)])
        );
        assert_eq!(
            "tags array-contains vip".parse::<Filter>().map(|f| f.op()),
            Ok(Op::ArrayContains)
        );
        assert!("status active".parse::<Filter>().is_err());
        assert!("id in a".parse::<Filter>().is_err());
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
            assert_eq!(f.op(), op, "{text}");
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
        assert_eq!(f.field().segments(), ["my field"]);
        let g: Filter = "address.city   ==   Tokyo".parse().unwrap();
        assert_eq!(g.field().segments(), ["address", "city"]);
        assert_eq!(g.value(), &Value::String("Tokyo".into()));
        let h: Filter = "`a == b` == 1".parse().unwrap();
        assert_eq!(h.field().segments(), ["a == b"]);
        let i: Filter = "`my-field` == 1".parse().unwrap();
        assert_eq!(i.field().segments(), ["my-field"]);
    }

    #[test]
    fn unquoted_fields_with_spaces_or_punctuation_are_rejected() {
        for bad in ["a b == c", "my-field == 1", "a:b == 1", "a+ == 1"] {
            let e = bad.parse::<Filter>().unwrap_err();
            assert!(e.reason.contains("backtick"), "{bad}: {e}");
        }
    }

    #[test]
    fn lists_only_where_an_array_is_expected() {
        assert!("age == [1, 2]".parse::<Filter>().is_err());
        assert!("tags array-contains [a]".parse::<Filter>().is_err());
        assert!("age < [1]".parse::<Filter>().is_err());
        assert_eq!(
            parts("tags array-contains-any [a, b]").2,
            Value::Array(vec!["a".into(), "b".into()])
        );
    }

    #[test]
    fn empty_lists_are_rejected() {
        for op in ["in", "not-in", "array-contains-any"] {
            assert!(format!("a {op} []").parse::<Filter>().is_err(), "{op}");
        }
    }

    #[test]
    fn errors_show_the_whole_input_once() {
        let e = "status active".parse::<Filter>().unwrap_err();
        assert_eq!(e.input, "status active");
        assert!(e.to_string().starts_with("status active: "), "{e}");
        let e = "when == ts:soon".parse::<Filter>().unwrap_err();
        assert_eq!(e.input, "when == ts:soon");
        assert!(!e.reason.contains("ts:soon:"), "{e}");
    }

    #[test]
    fn a_brace_value_stays_a_string_in_filters() {
        for text in [r#"payload == {"x":1}"#, r#"payload == "{\"x\":1}""#] {
            let f: Filter = text.parse().unwrap();
            assert_eq!(f.value(), &Value::String(r#"{"x":1}"#.into()), "{text}");
        }
    }

    proptest! {
        #![proptest_config(arb::config())]

        #[test]
        fn field_op_and_integer_parse_back_to_the_same_three_parts(
            field in arb::field_path(),
            op in prop::sample::select(Op::ALL.to_vec()),
            i in any::<i64>(),
            before in prop::sample::select(vec![" ", "  "]),
            after in prop::sample::select(vec![" ", "  "]),
        ) {
            let (value_text, value) = if op.takes_list() {
                (format!("[{i}]"), Value::Array(vec![Value::Int(i)]))
            } else {
                (i.to_string(), Value::Int(i))
            };
            let text = format!("{field}{before}{}{after}{value_text}", op.symbol());
            prop_assert_eq!(
                text.parse::<Filter>().map(|f| (f.field().clone(), f.op(), f.value().clone())),
                Ok((field, op, value))
            );
        }

        #[test]
        fn parsing_never_panics(
            field in arb::field_path(),
            op in prop::sample::select(Op::ALL.to_vec()),
            value in arb::text(),
            whole in arb::text(),
        ) {
            format!("{field} {} {value}", op.symbol()).parse::<Filter>().ok();
            whole.parse::<Filter>().ok();
        }
    }
}
