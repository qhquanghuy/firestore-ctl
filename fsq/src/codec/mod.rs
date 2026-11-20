//! Codecs between [`Value`](crate::domain::value::Value) /
//! [`Document`](crate::domain::document::Document) and JSON.
//!
//! * [`typed_json`] is lossless: what `get` prints and `set --data` reads back.
//! * [`plain_json`] is a one-way, untagged rendering for humans and `jq`.

use serde_json::Number;
use thiserror::Error;

use crate::domain::value::{Fields, Value};

pub mod plain_json;
pub mod typed_json;

/// Why a JSON input could not be decoded into Firestore values.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{}: {reason}", if at.is_empty() { "(root)" } else { at.as_str() })]
pub struct CodecError {
    /// RFC 6901 JSON pointer to the offending JSON node (`""` is the root).
    pub at: String,
    /// What is wrong with it.
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

/// Encodes each field with `encode` into a JSON object.
fn fields_to_object(
    fields: &Fields,
    encode: fn(&Value) -> serde_json::Value,
) -> serde_json::Map<String, serde_json::Value> {
    fields.iter().map(|(k, v)| (k.clone(), encode(v))).collect()
}

/// A finite double as a JSON number; NaN and infinities as the strings
/// `"NaN"`, `"Infinity"`, `"-Infinity"` (JSON has no such numbers).
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
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{plain_json, typed_json};
    use crate::domain::document::Document;
    use crate::domain::value::{Fields, GeoPoint, Timestamp, UpdateTime, Value};
    use serde_json::json;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn sample_doc_with(key: &str, v: Value) -> Document {
        Document {
            path: "teams/t1".parse().unwrap(),
            update_time: "2026-11-20T00:00:00Z".parse().unwrap(),
            data: [(key.to_string(), v)].into_iter().collect(),
        }
    }

    fn sample_doc(update_time: &str) -> Document {
        Document {
            update_time: update_time.parse::<UpdateTime>().unwrap(),
            ..sample_doc_with("name", "x".into())
        }
    }

    /// Text boundary: what `get` prints and `set --data` parses.
    fn through_text(v: &Value) -> Result<Value, super::CodecError> {
        let text = typed_json::value_to_json(v).to_string();
        typed_json::value_from_json(&serde_json::from_str(&text).unwrap())
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
        .map(|(k, v)| (k.to_string(), v))
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
    fn input_accepts_bare_data_or_full_document() {
        let full = typed_json::document_to_json(&sample_doc("2026-11-20T00:00:00Z"));
        assert_eq!(
            typed_json::fields_from_input(&full),
            typed_json::fields_from_input(&full["data"])
        );
    }

    #[test]
    fn user_field_named_path_does_not_collide() {
        let j = json!({"_path": "x", "data": {"_path": "mine"}});
        assert_eq!(
            typed_json::fields_from_input(&j).unwrap()["_path"],
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

    // ---- hardening beyond the brief ----

    #[test]
    fn int_extremes_roundtrip_through_text() {
        for i in [i64::MIN, i64::MAX, 0, -1] {
            assert_eq!(through_text(&Value::Int(i)), Ok(Value::Int(i)));
        }
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
        // serde_json itself refuses non-finite literals.
        assert!(serde_json::from_str::<serde_json::Value>("1e999").is_err());
    }

    #[test]
    fn out_of_range_integer_error_suggests_double_tag() {
        let j: serde_json::Value = serde_json::from_str("1e30").unwrap();
        let e = typed_json::value_from_json(&j).unwrap_err();
        assert!(e.reason.contains(r#"{"$double": n}"#), "{}", e.reason);
        // below 2^63 an exponent literal is a double
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
        // an array of maps containing arrays is fine
        assert!(typed_json::value_from_json(&json!([{"x": [1]}])).is_ok());
    }

    #[test]
    fn full_document_detection_is_strict() {
        // _updateTime alone marks a full document; a missing _path is not a flip to bare data
        let ok = typed_json::fields_from_input(
            &json!({"_updateTime": "2026-01-01T00:00:00Z", "data": {"a": 1}}),
        );
        assert_eq!(ok.unwrap()["a"], Value::Int(1));
        // data as the only key is a full document
        assert!(typed_json::fields_from_input(&json!({"data": {"a": 1}})).is_ok());
        assert_eq!(
            typed_json::fields_from_input(&json!({"data": 5}))
                .unwrap_err()
                .at,
            "/data"
        );
        // stray keys are errors, never dropped
        for j in [
            json!({"_path": "teams/t1", "data": {}, "extra": 1}),
            json!({"_path": "teams/t1", "extra": 1}),
            json!({"_updateTime": "2026-01-01T00:00:00Z", "extra": {}, "data": {}}),
        ] {
            let e = typed_json::fields_from_input(&j).unwrap_err();
            assert_eq!(e.at, "/extra", "{j}");
            assert!(
                e.reason.contains("extra") && e.reason.contains("inside `data`"),
                "{}",
                e.reason
            );
        }
        // a full document without data is an error
        assert_eq!(
            typed_json::fields_from_input(&json!({"_path": "teams/t1"}))
                .unwrap_err()
                .at,
            "/data"
        );
        // otherwise it is bare data, keys kept literally
        let bare = typed_json::fields_from_input(&json!({"data": 5, "other": 6})).unwrap();
        assert_eq!(bare.len(), 2);
    }

    #[test]
    fn doubles_roundtrip_bit_exact_through_text() {
        // xorshift-generated bit patterns, finite only
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
    fn negative_zero_keeps_its_sign() {
        assert!(matches!(
            through_text(&Value::Double(-0.0)),
            Ok(Value::Double(z)) if z.is_sign_negative()
        ));
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
    fn maps_with_a_single_dollar_key_roundtrip() {
        for key in ["$ts", "$foo", "$map", "$double"] {
            let m = Value::Map([(key.to_string(), Value::Int(1))].into_iter().collect());
            assert_eq!(through_text(&m), Ok(m.clone()), "{key}");
            let nested = Value::Array(vec![Value::Map(
                [(key.to_string(), m.clone())].into_iter().collect(),
            )]);
            assert_eq!(through_text(&nested), Ok(nested.clone()), "nested {key}");
        }
    }

    #[test]
    fn multi_key_map_with_dollar_key_is_plain_map() {
        let j = json!({"$ts": "x", "other": 1});
        assert!(matches!(typed_json::value_from_json(&j), Ok(Value::Map(m)) if m.len() == 2));
    }

    #[test]
    fn document_with_field_named_like_a_tag_roundtrips() {
        let doc = sample_doc_with("$ts", Value::Int(7));
        let j = typed_json::document_to_json(&doc);
        assert_eq!(typed_json::fields_from_input(&j), Ok(doc.data));
    }

    #[test]
    fn errors_carry_json_pointers() {
        let e = typed_json::value_from_json(&json!({"a": [1, {"$ts": "nope"}]})).unwrap_err();
        assert_eq!(e.at, "/a/1");
        let e = typed_json::fields_from_input(
            &json!({"_path": "x", "data": {"createdAt": {"$ts": 1}}}),
        )
        .unwrap_err();
        assert_eq!(e.at, "/data/createdAt");
        let e = typed_json::fields_from_input(&json!({"_path": "x", "data": 3})).unwrap_err();
        assert_eq!(e.at, "/data");
        let e = typed_json::fields_from_input(&json!([1])).unwrap_err();
        assert_eq!(e.at, "");
        let e = typed_json::fields_from_input(&json!({"a/b~c": {"$nope": 1}})).unwrap_err();
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
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        );
        assert_eq!(
            plain_json::value_to_json(&v),
            json!({"d": 1.5, "nan": "NaN", "b": "AP8=", "r": "teams/t1", "g": {"lat": 1.5, "lng": -2.0}, "i": 3})
        );
    }
}
