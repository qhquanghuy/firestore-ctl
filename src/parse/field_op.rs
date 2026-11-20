use std::str::FromStr;

use super::{ParseError, parse_field, parse_token, split_around, unquoted};
use crate::domain::{FieldOp, FieldPath, Value};

const FIELD_HINT: &str =
    "for a comparison use `==` in --where; backtick-quote unusual field names, e.g. `my-field`";

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
        let field_of = |text: &str| parse_field(text, FIELD_HINT).map_err(fail);
        let text = text.trim();
        if text.starts_with('=') {
            return Err(ParseError::new(
                s,
                "value starts with `=`; for a comparison use `==` in --where (quote the value to set it literally)",
            ));
        }
        let before = before.trim_end();
        let (name, op) = if let Some(name) = before.strip_suffix('+') {
            (name, Kind::Union)
        } else if let Some(name) = before.strip_suffix('-') {
            (name, Kind::Remove)
        } else {
            (before, Kind::Set)
        };
        let field = field_of(name)?;
        if text.starts_with('{') {
            return Err(ParseError::new(s, brace_hint(&field, op)));
        }
        let value = parse_token(text).map_err(|e| ParseError::new(s, e.reason))?;
        Ok(match op {
            Kind::Union => Self::ArrayUnion(field, elements(s, text, value)?),
            Kind::Remove => Self::ArrayRemove(field, elements(s, text, value)?),
            Kind::Set => Self::Set(field, value),
        })
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Set,
    Union,
    Remove,
}

// Examples are shell-quoted around the whole op: the shell would strip bare double quotes.
fn brace_hint(field: &FieldPath, kind: Kind) -> String {
    match kind {
        Kind::Set => format!(
            "--set cannot write a map; set nested fields one by one (--set '{field}.x=1'), \
             or quote the whole op to store text: --set '{field}=\"{{...}}\"'"
        ),
        Kind::Union => format!(
            "array elements cannot be maps; quote the whole op to store text: --set '{field}+=\"{{...}}\"'"
        ),
        Kind::Remove => format!(
            "array elements cannot be maps; quote the whole op to store text: --set '{field}-=\"{{...}}\"'"
        ),
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
    fn an_unquoted_value_starting_with_a_brace_is_rejected_with_a_hint_for_the_real_field() {
        let reason = |s: &str| s.parse::<FieldOp>().unwrap_err().reason;
        for bad in [r#"a.b={"x":1}"#, "a.b = {}"] {
            let r = reason(bad);
            assert!(r.contains("--set 'a.b.x=1'"), "{bad}: {r}");
            assert!(r.contains(r#"--set 'a.b="{...}"'"#), "{bad}: {r}");
        }
        for (bad, op) in [("tags+={", "+="), ("tags-={x}", "-=")] {
            let r = reason(bad);
            assert!(
                r.contains(&format!(r#"--set 'tags{op}"{{...}}"'"#)),
                "{bad}: {r}"
            );
            assert!(!r.contains("nested"), "{bad}: {r}");
        }
        assert!(!reason("a={}").contains("--merge"));
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
