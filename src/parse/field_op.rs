use std::str::FromStr;

use super::{ParseError, parse_field, parse_token, split_around, unquoted};
use crate::domain::{FieldOp, Value};

const FIELD_HINT: &str =
    "for a comparison use `==` in --where; backtick-quote unusual field names, e.g. `my-field`";

const BRACE_HINT: &str = r#"a value starting with `{` is not a map; to store it as text quote it, e.g. a="{\"x\":1}"; to write a map use firectl set <doc> --merge --data '{"a":{"x":1}}'"#;

impl FromStr for FieldOp {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, ParseError> {
        let fail = |reason: String| ParseError::new(s, reason);
        let (before, _, text) = unquoted(s, '`')
            .find(|&(_, c)| c == '=')
            .and_then(|(eq, _)| split_around(s, eq))
            .ok_or_else(|| {
                fail("expected `field=value`, `field+=value` or `field-=value`".to_owned())
            })?;
        let field = |text: &str| parse_field(text, FIELD_HINT).map_err(fail);
        let text = text.trim();
        if text.starts_with('=') {
            return Err(ParseError::new(
                s,
                "value starts with `=`; for a comparison use `==` in --where (quote the value to set it literally)",
            ));
        }
        if text.starts_with('{') {
            return Err(ParseError::new(s, BRACE_HINT));
        }
        let value = parse_token(text).map_err(|e| ParseError::new(s, e.reason))?;
        let before = before.trim_end();
        if let Some(name) = before.strip_suffix('+') {
            Ok(Self::ArrayUnion(field(name)?, elements(s, text, value)?))
        } else if let Some(name) = before.strip_suffix('-') {
            Ok(Self::ArrayRemove(field(name)?, elements(s, text, value)?))
        } else {
            Ok(Self::Set(field(before)?, value))
        }
    }
}

fn elements(input: &str, text: &str, value: Value) -> Result<Vec<Value>, ParseError> {
    let items = if let Value::Array(items) = value {
        items
    } else {
        vec![value]
    };
    if text.is_empty() || items.is_empty() {
        Err(ParseError::new(
            input,
            "`+=`/`-=` need at least one element, e.g. tags+=vip or tags-=[a, b]",
        ))
    } else {
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::FieldPath;

    fn fp(s: &str) -> FieldPath {
        s.parse().unwrap()
    }

    fn op(s: &str) -> FieldOp {
        s.parse().unwrap()
    }

    #[test]
    fn field_ops() {
        assert_eq!(op("plan=pro"), FieldOp::Set(fp("plan"), "pro".into()));
        assert_eq!(
            op("tags+=vip"),
            FieldOp::ArrayUnion(fp("tags"), vec!["vip".into()])
        );
        assert_eq!(
            op("tags-=[a, b]"),
            FieldOp::ArrayRemove(fp("tags"), vec!["a".into(), "b".into()])
        );
        assert_eq!(op("note=a=b"), FieldOp::Set(fp("note"), "a=b".into()));
    }

    #[test]
    fn set_takes_typed_values_including_lists() {
        assert_eq!(op("age=18"), FieldOp::Set(fp("age"), Value::Int(18)));
        assert_eq!(
            op("tags=[a, b]"),
            FieldOp::Set(fp("tags"), Value::Array(vec!["a".into(), "b".into()]))
        );
        assert_eq!(op("note="), FieldOp::Set(fp("note"), "".into()));
    }

    #[test]
    fn only_the_first_equals_decides_the_operator() {
        assert_eq!(op("note=a+=b"), FieldOp::Set(fp("note"), "a+=b".into()));
        assert_eq!(op("note=x-=y"), FieldOp::Set(fp("note"), "x-=y".into()));
    }

    #[test]
    fn spaces_around_the_operator_are_ignored() {
        assert_eq!(op("plan = pro"), FieldOp::Set(fp("plan"), "pro".into()));
        assert_eq!(
            op("tags += vip"),
            FieldOp::ArrayUnion(fp("tags"), vec!["vip".into()])
        );
        assert_eq!(
            op("tags + = vip"),
            FieldOp::ArrayUnion(fp("tags"), vec!["vip".into()])
        );
    }

    #[test]
    fn quoted_fields_allow_punctuation() {
        assert_eq!(
            op("`my-field`=3"),
            FieldOp::Set(fp("`my-field`"), Value::Int(3))
        );
        assert_eq!(op("`a=b`=1"), FieldOp::Set(fp("`a=b`"), Value::Int(1)));
        assert_eq!(op("`a+`=1"), FieldOp::Set(fp("`a+`"), Value::Int(1)));
        assert_eq!(
            op("`my field`=1"),
            FieldOp::Set(fp("`my field`"), Value::Int(1))
        );
    }

    #[test]
    fn ambiguous_unquoted_fields_are_rejected() {
        for bad in ["my-field=3", "x>=3", "x!=3", "x<=3", "a b=1", "a:b=1"] {
            assert!(bad.parse::<FieldOp>().is_err(), "{bad}");
        }
        let e = "x>=3".parse::<FieldOp>().unwrap_err();
        assert!(e.reason.contains("--where"), "{e}");
    }

    #[test]
    fn double_equals_is_not_a_set() {
        let e = "x==3".parse::<FieldOp>().unwrap_err();
        assert!(e.reason.contains("=="), "{e}");
        assert_eq!(op("x=\"=3\""), FieldOp::Set(fp("x"), "=3".into()));
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
    fn union_and_remove_need_elements() {
        for bad in ["tags+=", "tags+=[]", "tags-=", "tags-=[ ]"] {
            assert!(bad.parse::<FieldOp>().is_err(), "{bad}");
        }
        assert_eq!(
            op("tags+=\"\""),
            FieldOp::ArrayUnion(fp("tags"), vec!["".into()])
        );
    }

    #[test]
    fn union_of_a_quoted_string_is_one_element() {
        assert_eq!(
            op("tags+=\"[a]\""),
            FieldOp::ArrayUnion(fp("tags"), vec!["[a]".into()])
        );
    }

    #[test]
    fn errors_show_the_whole_input_once() {
        let e = "when=ts:soon".parse::<FieldOp>().unwrap_err();
        assert_eq!(e.input, "when=ts:soon");
        assert!(!e.reason.contains("ts:soon:"), "{e}");
    }

    #[test]
    fn an_unquoted_value_starting_with_a_brace_is_rejected_with_both_ways_out() {
        for bad in [r#"a={"x":1}"#, "a+={", "a-={x}", "a = {}"] {
            let e = bad.parse::<FieldOp>().unwrap_err();
            assert!(e.reason.contains(r#"a="{\"x\":1}""#), "{bad}: {e}");
            assert!(
                e.reason
                    .contains(r#"firectl set <doc> --merge --data '{"a":{"x":1}}'"#),
                "{bad}: {e}"
            );
        }
    }

    #[test]
    fn the_suggested_quoted_form_stores_the_string() {
        assert_eq!(
            op(r#"a="{\"x\":1}""#),
            FieldOp::Set(fp("a"), Value::String(r#"{"x":1}"#.into()))
        );
        assert_eq!(
            op(r#"a+="{x}""#),
            FieldOp::ArrayUnion(fp("a"), vec!["{x}".into()])
        );
    }

    #[test]
    fn a_brace_after_the_first_character_is_text() {
        assert_eq!(op("a=x{"), FieldOp::Set(fp("a"), "x{".into()));
        assert_eq!(op("a= x{}"), FieldOp::Set(fp("a"), "x{}".into()));
    }
}
