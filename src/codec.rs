use serde_json::Number;
use thiserror::Error;

use crate::domain::field::FieldPath;
use crate::domain::value::{Fields, Value};

pub mod plain_json;
pub mod typed_json;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{}: {reason}", if at.is_empty() { "(root)" } else { at.as_str() })]
pub struct CodecError {
    pub at: String,
    pub reason: String,
}

impl CodecError {
    fn new(at: &str, reason: impl Into<String>) -> Self {
        Self {
            at: at.to_owned(),
            reason: reason.into(),
        }
    }
}

fn fields_to_object(
    fields: &Fields,
    encode: fn(&Value) -> serde_json::Value,
) -> serde_json::Map<String, serde_json::Value> {
    fields.iter().map(|(k, v)| (k.clone(), encode(v))).collect()
}

/// `set --overwrite` refuses input carrying this marker: it holds only some of the fields.
#[must_use]
pub fn mark_selected(document: serde_json::Value, select: &[FieldPath]) -> serde_json::Value {
    if let serde_json::Value::Object(object) = document {
        serde_json::Value::Object(
            object
                .into_iter()
                .chain([(
                    "_select".to_owned(),
                    select.iter().map(ToString::to_string).collect(),
                )])
                .collect(),
        )
    } else {
        document
    }
}

/// NaN and infinities are strings: JSON has no such numbers.
fn double_json(f: f64) -> serde_json::Value {
    Number::from_f64(f).map_or_else(
        || {
            serde_json::Value::String(
                if f.is_nan() {
                    "NaN"
                } else if f.is_sign_positive() {
                    "Infinity"
                } else {
                    "-Infinity"
                }
                .to_owned(),
            )
        },
        serde_json::Value::Number,
    )
}

#[cfg(test)]
mod tests {
    use super::{plain_json, typed_json};
    use crate::domain::arb;
    use crate::domain::document::Document;
    use crate::domain::path::DocPath;
    use crate::domain::value::{Fields, GeoPoint, Timestamp, UpdateTime, Value};
    use proptest::collection::{btree_map, vec};
    use proptest::prelude::*;
    use proptest::sample::select;
    use serde_json::Value as Json;
    use serde_json::json;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn sample_doc_with(key: &str, v: Value) -> Document {
        Document {
            path: "teams/t1".parse().unwrap(),
            update_time: "2026-11-20T00:00:00Z".parse().unwrap(),
            data: [(key.to_owned(), v)].into_iter().collect(),
        }
    }

    fn sample_doc(update_time: &str) -> Document {
        Document {
            update_time: update_time.parse::<UpdateTime>().unwrap(),
            ..sample_doc_with("name", "x".into())
        }
    }

    fn fields_from_input(j: &serde_json::Value) -> Result<Fields, super::CodecError> {
        typed_json::parse_input(j).map(|input| input.fields)
    }

    fn parse_text(text: &str) -> serde_json::Value {
        serde_json::from_str(text).unwrap()
    }

    fn through_text(v: &Value) -> Result<Value, super::CodecError> {
        let text = typed_json::value_to_json(v).to_string();
        typed_json::value_from_json(&parse_text(&text))
    }

    #[test]
    fn roundtrip_all_types() {
        let fields: Fields = [
            ("s", "x".into()),
            ("bo", Value::Bool(true)),
            ("i", Value::Int(9_007_199_254_740_993)),
            ("d", Value::Double(1.0)),
            ("t", Value::Timestamp(ts("2026-01-05T10:00:00.123456789Z"))),
            ("r", Value::Ref("teams/t1".parse().unwrap())),
            ("b", Value::Bytes(vec![0, 255])),
            ("g", Value::Geo(GeoPoint::new(1.5, -2.0).unwrap())),
            ("n", Value::Null),
            ("a", Value::Array(vec![Value::Int(1)])),
            ("m", Value::Map(Fields::new())),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        let json = typed_json::value_to_json(&Value::Map(fields.clone()));
        assert_eq!(typed_json::value_from_json(&json), Ok(Value::Map(fields)));
        assert_eq!(json["i"], json!(9_007_199_254_740_993_i64));
        assert_eq!(json["d"], json!({"$double": 1.0}));
    }

    #[test]
    fn update_time_roundtrips_nanos() {
        let doc = sample_doc("2026-11-20T03:12:45.123456789Z");
        let j = typed_json::document_to_json(&doc);
        assert_eq!(j["_updateTime"], "2026-11-20T03:12:45.123456789Z");
    }

    #[test]
    fn user_field_named_path_does_not_collide() {
        let j = json!({"_path": "teams/t1", "data": {"_path": "mine"}});
        assert_eq!(
            fields_from_input(&j).unwrap()["_path"],
            Value::String("mine".into())
        );
    }

    #[test]
    fn unknown_tag_is_error() {
        assert!(typed_json::value_from_json(&json!({"$tz": "x"})).is_err());
    }

    #[test]
    fn plain_is_untagged() {
        let j = plain_json::document_to_json(&sample_doc_with(
            "t",
            Value::Timestamp(ts("2026-01-05T10:00:00Z")),
        ));
        assert_eq!(j["data"]["t"], "2026-01-05T10:00:00Z");
    }

    #[test]
    fn integers_outside_i64_are_errors_not_doubles() {
        for lit in [
            "9223372036854775808",
            "18446744073709551616",
            "-9223372036854775809",
            "123456789012345678901234567890",
            "1e30",
            "9223372036854775808.0",
        ] {
            let j: serde_json::Value = serde_json::from_str(lit).unwrap();
            assert!(typed_json::value_from_json(&j).is_err(), "{lit}");
        }
    }

    #[test]
    fn fractional_and_exponent_numbers_are_doubles() {
        for (lit, want) in [("1e3", 1000.0), ("1.0", 1.0), ("-2.5E-1", -0.25)] {
            let j: serde_json::Value = serde_json::from_str(lit).unwrap();
            assert_eq!(
                typed_json::value_from_json(&j),
                Ok(Value::Double(want)),
                "{lit}"
            );
        }
        assert!(serde_json::from_str::<serde_json::Value>("1e999").is_err());
    }

    #[test]
    fn out_of_range_integer_error_suggests_double_tag() {
        let j: serde_json::Value = serde_json::from_str("1e30").unwrap();
        let e = typed_json::value_from_json(&j).unwrap_err();
        assert!(e.reason.contains(r#"{"$double": n}"#), "{}", e.reason);
        let j: serde_json::Value = serde_json::from_str("9e18").unwrap();
        assert_eq!(typed_json::value_from_json(&j), Ok(Value::Double(9e18)));
    }

    #[test]
    fn minus_zero_literal_is_the_double_negative_zero() {
        let j: serde_json::Value = serde_json::from_str("-0").unwrap();
        assert!(matches!(
            typed_json::value_from_json(&j),
            Ok(Value::Double(z)) if z == 0.0 && z.is_sign_negative()
        ));
    }

    #[test]
    fn nested_arrays_are_rejected_with_a_pointer() {
        let e = typed_json::value_from_json(&json!({"a": [1, [2]]})).unwrap_err();
        assert_eq!(e.at, "/a/1");
        assert!(typed_json::value_from_json(&json!([{"x": [1]}])).is_ok());
    }

    #[test]
    fn full_document_detection_is_strict() {
        let ok =
            fields_from_input(&json!({"_updateTime": "2026-01-01T00:00:00Z", "data": {"a": 1}}));
        assert_eq!(ok.unwrap()["a"], Value::Int(1));
        let field_named_data = fields_from_input(&json!({"data": {"a": 1}})).unwrap();
        assert_eq!(field_named_data.len(), 1);
        assert_eq!(
            field_named_data["data"],
            Value::Map([("a".to_owned(), Value::Int(1))].into_iter().collect())
        );
        assert_eq!(
            fields_from_input(&json!({"data": 5})).unwrap()["data"],
            Value::Int(5)
        );
        for j in [
            json!({"_path": "teams/t1", "data": {}, "extra": 1}),
            json!({"_path": "teams/t1", "extra": 1}),
            json!({"_updateTime": "2026-01-01T00:00:00Z", "extra": {}, "data": {}}),
        ] {
            let e = fields_from_input(&j).unwrap_err();
            assert_eq!(e.at, "/extra", "{j}");
            assert!(
                e.reason.contains("extra") && e.reason.contains("inside `data`"),
                "{}",
                e.reason
            );
        }
        assert_eq!(
            fields_from_input(&json!({"_path": "teams/t1"}))
                .unwrap_err()
                .at,
            "/data"
        );
        let bare = fields_from_input(&json!({"data": 5, "other": 6})).unwrap();
        assert_eq!(bare.len(), 2);
    }

    #[test]
    fn parse_input_reports_the_envelope_of_a_full_document() {
        let t: UpdateTime = "2026-01-05T10:00:00.123456789Z".parse().unwrap();
        let input = typed_json::parse_input(&json!({
            "_path": "teams/t1",
            "_updateTime": "2026-01-05T10:00:00.123456789Z",
            "data": {"a": 1}
        }))
        .unwrap();
        let envelope = input.envelope.unwrap();
        assert_eq!(envelope.path, Some("teams/t1".parse().unwrap()));
        assert_eq!(envelope.update_time, Some(t));
        assert_eq!((envelope.plain, envelope.select), (false, None));
        assert!(
            typed_json::parse_input(&json!({"a": 1}))
                .unwrap()
                .envelope
                .is_none()
        );
        for (j, at) in [
            (json!({"_path": 7, "data": {}}), "/_path"),
            (json!({"_path": "teams", "data": {}}), "/_path"),
            (
                json!({"_updateTime": "yesterday", "data": {}}),
                "/_updateTime",
            ),
            (
                json!({"_updateTime": "2026-01-01T00:00:00Z", "_select": 1, "data": {}}),
                "/_select",
            ),
            (
                json!({"_updateTime": "2026-01-01T00:00:00Z", "_select": [3], "data": {}}),
                "/_select/0",
            ),
        ] {
            assert_eq!(typed_json::parse_input(&j).unwrap_err().at, at, "{j}");
        }
    }

    #[test]
    fn output_markers_are_envelope_not_data() {
        let input = typed_json::parse_input(&json!({
            "_path": "teams/t1",
            "_updateTime": "2026-01-01T00:00:00Z",
            "_plain": true,
            "_select": ["a", "`b c`.d"],
            "data": {"a": 1}
        }))
        .unwrap();
        let envelope = input.envelope.unwrap();
        assert!(envelope.plain);
        assert_eq!(
            envelope.select,
            Some(vec!["a".parse().unwrap(), "`b c`.d".parse().unwrap()])
        );
        assert_eq!(input.fields.keys().collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn a_marker_alone_makes_a_full_document() {
        let stripped = typed_json::parse_input(&json!({
            "_plain": true,
            "_select": ["a"],
            "data": {"a": 1}
        }))
        .unwrap();
        let envelope = stripped.envelope.unwrap();
        assert!(envelope.plain);
        assert_eq!(envelope.select.map(|s| s.len()), Some(1));
        assert_eq!(envelope.path, None);
        assert_eq!(stripped.fields.keys().collect::<Vec<_>>(), ["a"]);
        for marked in [
            json!({"_plain": true, "a": 1}),
            json!({"_select": ["a"], "a": 1}),
        ] {
            assert!(
                typed_json::parse_input(&marked)
                    .unwrap_err()
                    .to_string()
                    .contains("inside `data`"),
                "{marked}"
            );
        }
        assert_eq!(
            typed_json::parse_input(&json!({"a": 1})).unwrap().envelope,
            None
        );
    }

    #[test]
    fn plain_documents_say_so_and_selected_ones_list_their_fields() {
        let doc = sample_doc("2026-11-20T00:00:00Z");
        assert_eq!(plain_json::document_to_json(&doc)["_plain"], json!(true));
        assert!(typed_json::document_to_json(&doc).get("_plain").is_none());
        let marked = super::mark_selected(
            typed_json::document_to_json(&doc),
            &["name".parse().unwrap(), "a.`b c`".parse().unwrap()],
        );
        assert_eq!(marked["_select"], json!(["name", "a.`b c`"]));
        let read = typed_json::parse_input(&marked).unwrap();
        assert_eq!(read.fields, doc.data);
        assert_eq!(read.envelope.unwrap().select.map(|s| s.len()), Some(2));
    }

    const FOREIGN: &str = "projects/other/databases/(default)/documents/users/u1";

    #[test]
    fn foreign_references_roundtrip_through_typed_json() {
        let v = Value::ForeignRef(FOREIGN.parse().unwrap());
        assert_eq!(typed_json::value_to_json(&v), json!({"$ref": FOREIGN}));
        assert_eq!(through_text(&v), Ok(v));
        assert_eq!(
            typed_json::value_from_json(&json!({"$ref": "users/u1"})),
            Ok(Value::Ref("users/u1".parse().unwrap()))
        );
    }

    #[test]
    fn ref_to_a_collection_named_projects_stays_relative() {
        for rel in ["projects/p1", "projects/p/databases/d/documents/c"] {
            let v = Value::Ref(rel.parse().unwrap());
            assert_eq!(through_text(&v), Ok(v), "{rel}");
        }
        assert!(typed_json::value_from_json(&json!({"$ref": "projects/p/x"})).is_err());
    }

    #[test]
    fn doubles_roundtrip_bit_exact_through_text() {
        let bits = std::iter::successors(Some(0x9E37_79B9_7F4A_7C15_u64), |x| {
            let a = x ^ (x << 13);
            let b = a ^ (a >> 7);
            Some(b ^ (b << 17))
        })
        .take(5000)
        .map(f64::from_bits)
        .filter(|f| f.is_finite());
        for f in bits.chain([0.1, f64::MIN_POSITIVE, f64::MAX, 5e-324, -0.0]) {
            assert!(
                matches!(through_text(&Value::Double(f)), Ok(Value::Double(g)) if g.to_bits() == f.to_bits()),
                "{f:e}"
            );
        }
    }

    #[test]
    fn non_finite_doubles_roundtrip() {
        for f in [f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(through_text(&Value::Double(f)), Ok(Value::Double(f)));
        }
        assert_eq!(
            typed_json::value_to_json(&Value::Double(f64::NAN)),
            json!({"$double": "NaN"})
        );
        assert!(
            matches!(through_text(&Value::Double(f64::NAN)), Ok(Value::Double(n)) if n.is_nan())
        );
    }

    #[test]
    fn double_tag_accepts_integer_and_rejects_other_strings() {
        assert_eq!(
            typed_json::value_from_json(&json!({"$double": 1})),
            Ok(Value::Double(1.0))
        );
        assert!(typed_json::value_from_json(&json!({"$double": "nan"})).is_err());
        assert!(typed_json::value_from_json(&json!({"$double": true})).is_err());
    }

    #[test]
    fn multi_key_map_with_dollar_key_is_plain_map() {
        let j = json!({"$ts": "x", "other": 1});
        assert!(matches!(typed_json::value_from_json(&j), Ok(Value::Map(m)) if m.len() == 2));
    }

    #[test]
    fn errors_carry_json_pointers() {
        let e = typed_json::value_from_json(&json!({"a": [1, {"$ts": "nope"}]})).unwrap_err();
        assert_eq!(e.at, "/a/1");
        let e = fields_from_input(&json!({"_path": "teams/t1", "data": {"createdAt": {"$ts": 1}}}))
            .unwrap_err();
        assert_eq!(e.at, "/data/createdAt");
        let e = fields_from_input(&json!({"_path": "teams/t1", "data": 3})).unwrap_err();
        assert_eq!(e.at, "/data");
        let e = fields_from_input(&json!([1])).unwrap_err();
        assert_eq!(e.at, "");
        let e = fields_from_input(&json!({"a/b~c": {"$nope": 1}})).unwrap_err();
        assert_eq!(e.at, "/a~1b~0c");
    }

    #[test]
    fn geo_and_bytes_and_ref_reject_bad_input() {
        for j in [
            json!({"$geo": {"lat": 91.0, "lng": 0.0}}),
            json!({"$geo": {"lat": 1.0}}),
            json!({"$geo": {"lat": 1.0, "lng": 2.0, "alt": 3.0}}),
            json!({"$geo": [1, 2]}),
            json!({"$bytes": "!!!"}),
            json!({"$bytes": 5}),
            json!({"$ref": "teams"}),
            json!({"$ts": "yesterday"}),
        ] {
            assert!(typed_json::value_from_json(&j).is_err(), "{j}");
        }
    }

    #[test]
    fn plain_renders_every_type_untagged() {
        let v = Value::Map(
            [
                ("d", Value::Double(1.5)),
                ("nan", Value::Double(f64::NAN)),
                ("b", Value::Bytes(vec![0, 255])),
                ("r", Value::Ref("teams/t1".parse().unwrap())),
                ("g", Value::Geo(GeoPoint::new(1.5, -2.0).unwrap())),
                ("i", Value::Int(3)),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
        );
        assert_eq!(
            plain_json::value_to_json(&v),
            json!({"d": 1.5, "nan": "NaN", "b": "AP8=", "r": "teams/t1", "g": {"lat": 1.5, "lng": -2.0}, "i": 3})
        );
    }

    const ENVELOPE_KEYS: [&str; 4] = ["_path", "_updateTime", "_plain", "_select"];

    fn json_key() -> impl Strategy<Value = String> {
        let special = select(vec![
            "$ts",
            "$ref",
            "$double",
            "$bytes",
            "$geo",
            "$map",
            "$nope",
            "_path",
            "_updateTime",
            "_plain",
            "_select",
            "data",
            "lat",
            "lng",
        ])
        .prop_map(str::to_owned);
        prop_oneof![3 => special, 1 => arb::text()]
    }

    // Biased to what the decoder special-cases: tags, envelope keys, `u64` above `i64::MAX`,
    // integral doubles beyond `i64`, and strings a tag accepts. An array in an array is legal
    // input here, because the decoder must reject it, not panic.
    fn arbitrary_json() -> impl Strategy<Value = Json> {
        let strings = prop_oneof![
            3 => arb::text(),
            2 => select(vec![
                "NaN",
                "Infinity",
                "-Infinity",
                "AP8=",
                "!!!",
                "2026-01-05T10:00:00Z",
                "teams/t1",
                "teams",
                "projects/p/databases/d/documents/users/u1",
                "a.`b c`",
            ])
            .prop_map(str::to_owned),
        ];
        let numbers = prop_oneof![
            any::<i64>().prop_map(Json::from),
            ((1_u64 << 63)..=u64::MAX).prop_map(Json::from),
            any::<f64>().prop_map(Json::from),
            select(vec![1e30, 9.3e18, -0.0, 1.0, 5e-324, 91.0, -181.0]).prop_map(Json::from),
        ];
        let tagged = prop_oneof![
            select(vec!["NaN", "Infinity", "-Infinity"]).prop_map(|s| json!({"$double": s})),
            Just(json!({"$ts": "2026-01-05T10:00:00.123456789Z"})),
            Just(json!({"$ref": "teams/t1"})),
            Just(json!({"$ref": "projects/p/databases/d/documents/users/u1"})),
            Just(json!({"$bytes": "AP8="})),
            (-100.0..100.0_f64, -200.0..200.0_f64)
                .prop_map(|(lat, lng)| json!({"$geo": {"lat": lat, "lng": lng}})),
        ];
        let leaf = prop_oneof![
            1 => Just(Json::Null),
            1 => any::<bool>().prop_map(Json::Bool),
            3 => numbers,
            3 => strings.prop_map(Json::String),
            2 => tagged,
        ];
        leaf.prop_recursive(3, 24, 6, |inner| {
            prop_oneof![
                vec(inner.clone(), 0..=6).prop_map(Json::Array),
                btree_map(json_key(), inner, 0..=4)
                    .prop_map(|entries| Json::Object(entries.into_iter().collect())),
            ]
        })
    }

    // A document path that starts with `projects/` must not be read as a resource name.
    fn projects_ref() -> impl Strategy<Value = DocPath> {
        let prefix = select(vec!["projects/x/", "projects/p/databases/d/"]);
        (prefix, arb::doc_path()).prop_filter_map("a document path", |(prefix, p)| {
            format!("{prefix}{p}").parse().ok()
        })
    }

    fn walk_json<'a>(j: &'a Json, depth: usize, out: &mut Vec<(usize, &'a Json)>) {
        out.push((depth, j));
        match j {
            Json::Array(xs) => {
                for x in xs {
                    walk_json(x, depth + 1, out);
                }
            }
            Json::Object(o) => {
                for x in o.values() {
                    walk_json(x, depth + 1, out);
                }
            }
            Json::Null | Json::Bool(_) | Json::Number(_) | Json::String(_) => {}
        }
    }

    #[test]
    fn projects_ref_reaches_both_prefixes() {
        use proptest::strategy::ValueTree as _;
        use proptest::test_runner::TestRunner;

        let strategy = projects_ref();
        let mut runner = TestRunner::deterministic();
        let paths: Vec<String> = (0..1_000)
            .map(|_| {
                strategy
                    .new_tree(&mut runner)
                    .unwrap()
                    .current()
                    .to_string()
            })
            .collect();
        for prefix in ["projects/x/", "projects/p/databases/d/"] {
            assert!(
                paths.iter().any(|p| p.starts_with(prefix)),
                "no sample with prefix: {prefix}"
            );
        }
    }

    #[test]
    fn arbitrary_json_reaches_its_classes() {
        use proptest::strategy::ValueTree as _;
        use proptest::test_runner::TestRunner;

        let strategy = arbitrary_json();
        let mut runner = TestRunner::deterministic();
        let trees: Vec<Json> = (0..1_000)
            .map(|_| strategy.new_tree(&mut runner).unwrap().current())
            .collect();
        let mut nodes = Vec::new();
        for tree in &trees {
            walk_json(tree, 0, &mut nodes);
        }
        let reaches = |class: &str, is_member: &dyn Fn(&(usize, &Json)) -> bool| {
            assert!(nodes.iter().any(is_member), "no sample in class: {class}");
        };
        let has_key = |j: &Json, key: &str| j.as_object().is_some_and(|o| o.contains_key(key));
        reaches("object with an envelope key", &|&(_, j)| {
            ENVELOPE_KEYS.iter().any(|k| has_key(j, k))
        });
        reaches("envelope key beside a data object", &|&(_, j)| {
            ENVELOPE_KEYS.iter().any(|k| has_key(j, k))
                && j.get("data").is_some_and(Json::is_object)
        });
        reaches("sole tag object", &|&(_, j)| {
            j.as_object()
                .is_some_and(|o| o.len() == 1 && o.keys().all(|k| k.starts_with('$')))
        });
        reaches("number above i64::MAX", &|&(_, j)| {
            j.is_u64() && j.as_i64().is_none()
        });
        reaches("integral double beyond i64", &|&(_, j)| {
            j.is_f64()
                && j.as_f64()
                    .is_some_and(|f| f.fract() == 0.0 && f.abs() >= 9.2e18)
        });
        reaches("untagged non-integral double", &|&(_, j)| {
            j.is_f64() && j.as_f64().is_some_and(|f| f.fract() != 0.0)
        });
        reaches("array directly inside an array", &|&(_, j)| {
            j.as_array().is_some_and(|xs| xs.iter().any(Json::is_array))
        });
        reaches("string a tag accepts", &|&(_, j)| {
            matches!(j.as_str(), Some("NaN" | "Infinity" | "-Infinity" | "AP8="))
        });
        reaches("object nested two deep", &|&(depth, j)| {
            depth == 2 && j.is_object()
        });
        let decoded: Vec<_> = trees.iter().map(typed_json::parse_input).collect();
        assert!(decoded.iter().any(Result::is_ok), "no input decodes");
        assert!(decoded.iter().any(Result::is_err), "no input is rejected");
        let values: Vec<_> = trees.iter().map(typed_json::value_from_json).collect();
        assert!(
            values.iter().any(|v| matches!(v, Ok(Value::Map(_)))),
            "no object decodes to a map"
        );
        assert!(
            values.iter().any(|v| matches!(
                v,
                Ok(Value::Timestamp(_) | Value::Ref(_) | Value::Bytes(_) | Value::Geo(_))
            )),
            "no tag decodes"
        );
    }

    proptest! {
        #![proptest_config(arb::config())]

        #[test]
        fn a_value_round_trips_in_memory_and_through_text(v in arb::value()) {
            let direct = typed_json::value_from_json(&typed_json::value_to_json(&v));
            prop_assert!(direct.as_ref().is_ok_and(|back| arb::same(back, &v)), "{direct:?}");
            let text = through_text(&v);
            prop_assert!(text.as_ref().is_ok_and(|back| arb::same(back, &v)), "{text:?}");
        }

        #[test]
        fn a_ref_under_projects_stays_a_ref(path in projects_ref()) {
            let v = Value::Ref(path);
            let text = through_text(&v);
            prop_assert_eq!(text, Ok(v));
        }

        #[test]
        fn a_full_document_round_trips_without_markers(d in arb::document()) {
            let input = typed_json::parse_input(&typed_json::document_to_json(&d));
            prop_assert!(
                input.as_ref().is_ok_and(|i| {
                    arb::same_fields(&i.fields, &d.data)
                        && i.envelope.as_ref().is_some_and(|e| {
                            e.path.as_ref() == Some(&d.path)
                                && e.update_time == Some(d.update_time)
                                && !e.plain
                                && e.select.is_none()
                        })
                }),
                "{input:?}"
            );
        }

        #[test]
        fn a_selection_marker_survives_beside_the_fields(
            d in arb::document(),
            select in vec(arb::field_path(), 0..=3),
        ) {
            let marked = super::mark_selected(typed_json::document_to_json(&d), &select);
            let input = typed_json::parse_input(&marked);
            prop_assert!(
                input.as_ref().is_ok_and(|i| {
                    arb::same_fields(&i.fields, &d.data)
                        && i.envelope.as_ref().is_some_and(|e| e.select.as_ref() == Some(&select))
                }),
                "{input:?}"
            );
        }

        #[test]
        fn plain_output_carries_the_plain_marker_through_parsing(d in arb::document()) {
            let json = plain_json::document_to_json(&d);
            prop_assert_eq!(&json["_plain"], &Json::Bool(true));
            if let Ok(input) = typed_json::parse_input(&json) {
                prop_assert!(input.envelope.is_some_and(|e| e.plain), "{json}");
            }
        }

        #[test]
        fn bare_data_has_no_envelope(fields in arb::fields()) {
            let fields: Fields = fields
                .into_iter()
                .filter(|(key, _)| !ENVELOPE_KEYS.contains(&key.as_str()))
                .collect();
            let bare = Json::Object(super::fields_to_object(&fields, typed_json::value_to_json));
            let input = typed_json::parse_input(&bare);
            prop_assert!(
                input.as_ref().is_ok_and(|i| {
                    i.envelope.is_none() && arb::same_fields(&i.fields, &fields)
                }),
                "{input:?}"
            );
        }

        #[test]
        fn decoding_arbitrary_json_returns(j in arbitrary_json()) {
            typed_json::parse_input(&j).ok();
            typed_json::value_from_json(&j).ok();
        }

        #[test]
        fn a_decoded_value_encodes_back_to_itself(j in arbitrary_json()) {
            if let Ok(v) = typed_json::value_from_json(&j) {
                let again = typed_json::value_from_json(&typed_json::value_to_json(&v));
                prop_assert!(again.as_ref().is_ok_and(|a| arb::same(a, &v)), "{j} -> {again:?}");
            }
        }
    }
}
