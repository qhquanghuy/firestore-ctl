//! Lossless typed JSON: the format `get` prints and `set --data` reads back.
//!
//! | Firestore | JSON |
//! |---|---|
//! | null, bool, string | as is |
//! | integer | JSON integer (never passes through `f64`) |
//! | double | `{"$double": 1.5}`; NaN/infinities `{"$double": "NaN" \| "Infinity" \| "-Infinity"}` |
//! | timestamp | `{"$ts": "2026-01-05T10:00:00.123456789Z"}` (RFC 3339, full nanoseconds) |
//! | reference | `{"$ref": "teams/t1"}` (relative to the current database) |
//! | bytes | `{"$bytes": "<standard base64 with padding>"}` |
//! | geo point | `{"$geo": {"lat": 1.5, "lng": -2.0}}` |
//! | array, map | JSON array, JSON object |
//!
//! Decoding rules for objects:
//! * An object with exactly one key that starts with `$` must be a known tag;
//!   an unknown one is an error (a typo must not silently become a map).
//! * Objects with several keys are plain maps, even if one key starts with `$`.
//! * To keep the round trip exact, a map whose *only* key starts with `$` is
//!   written as `{"$map": {...}}`; reading `$map` yields that inner object as a map.
//!
//! A JSON number with a fraction or exponent decodes as a double, any other as an
//! integer (an error outside `i64`, never a silent double).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Number, Value as Json, json};

use super::{CodecError, double_json};
use crate::domain::document::Document;
use crate::domain::value::{Fields, GeoPoint, Timestamp, Value};

/// Encodes a value as typed JSON.
#[must_use]
pub fn value_to_json(v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Int(i) => Json::from(*i),
        Value::Double(f) => json!({"$double": double_json(*f)}),
        Value::String(s) => Json::String(s.clone()),
        Value::Timestamp(t) => json!({"$ts": t.to_string()}),
        Value::Bytes(b) => json!({"$bytes": STANDARD.encode(b)}),
        Value::Ref(p) => json!({"$ref": p.to_string()}),
        Value::Geo(g) => {
            json!({"$geo": {"lat": double_json(g.lat()), "lng": double_json(g.lng())}})
        }
        Value::Array(xs) => Json::Array(xs.iter().map(value_to_json).collect()),
        Value::Map(m) => map_to_json(m),
    }
}

fn fields_to_object(fields: &Fields) -> Map<String, Json> {
    fields
        .iter()
        .map(|(k, v)| (k.clone(), value_to_json(v)))
        .collect()
}

/// A map value; escaped as `$map` when it would otherwise look like a tag.
fn map_to_json(fields: &Fields) -> Json {
    let object = fields_to_object(fields);
    if object.len() == 1 && object.keys().all(|k| k.starts_with('$')) {
        json!({"$map": object})
    } else {
        Json::Object(object)
    }
}

/// Encodes a document as `{"_path", "_updateTime", "data"}`; the top-level
/// `data` object is the document's fields (not escaped like a map value).
#[must_use]
pub fn document_to_json(d: &Document) -> Json {
    json!({
        "_path": d.path.to_string(),
        "_updateTime": d.update_time.to_string(),
        "data": Json::Object(fields_to_object(&d.data)),
    })
}

/// Decodes typed JSON into a value.
///
/// # Errors
/// [`CodecError`] pointing at the first invalid node.
pub fn value_from_json(j: &Json) -> Result<Value, CodecError> {
    decode(j, "")
}

/// Reads the fields of a document from `set --data` input: either a bare data
/// object, or a full document object (recognised by having both `_path` and
/// `data` keys), in which case only its `data` is used. A bare object that
/// happens to have both a `_path` and a `data` field is therefore read as a
/// full document.
///
/// # Errors
/// [`CodecError`] if the input is not an object, `data` is not an object, or a
/// field fails to decode.
pub fn fields_from_input(j: &Json) -> Result<Fields, CodecError> {
    let top = j
        .as_object()
        .ok_or_else(|| CodecError::new("", "expected a JSON object"))?;
    match (top.get("_path"), top.get("data")) {
        (Some(_), Some(data)) => data
            .as_object()
            .ok_or_else(|| CodecError::new("/data", "`data` must be a JSON object"))
            .and_then(|o| decode_fields(o, "/data")),
        _ => decode_fields(top, ""),
    }
}

/// Appends one escaped segment (RFC 6901) to a JSON pointer.
fn child(at: &str, segment: &str) -> String {
    format!("{at}/{}", segment.replace('~', "~0").replace('/', "~1"))
}

fn decode_fields(object: &Map<String, Json>, at: &str) -> Result<Fields, CodecError> {
    object
        .iter()
        .map(|(k, v)| Ok((k.clone(), decode(v, &child(at, k))?)))
        .collect()
}

fn decode(j: &Json, at: &str) -> Result<Value, CodecError> {
    match j {
        Json::Null => Ok(Value::Null),
        Json::Bool(b) => Ok(Value::Bool(*b)),
        Json::String(s) => Ok(Value::String(s.clone())),
        Json::Number(n) => decode_number(n, at),
        Json::Array(xs) => xs
            .iter()
            .enumerate()
            .map(|(i, x)| decode(x, &child(at, &i.to_string())))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Json::Object(o) => match single_entry(o) {
            Some((tag, inner)) if tag.starts_with('$') => decode_tag(tag, inner, at),
            _ => decode_fields(o, at).map(Value::Map),
        },
    }
}

fn single_entry(o: &Map<String, Json>) -> Option<(&String, &Json)> {
    o.iter().next().filter(|_| o.len() == 1)
}

fn decode_number(n: &Number, at: &str) -> Result<Value, CodecError> {
    if let Some(i) = n.as_i64() {
        Ok(Value::Int(i))
    } else if n.to_string().contains(['.', 'e', 'E']) {
        finite(n, at).map(Value::Double)
    } else {
        Err(CodecError::new(
            at,
            format!("integer {n} is outside the 64-bit range"),
        ))
    }
}

fn finite(n: &Number, at: &str) -> Result<f64, CodecError> {
    n.as_f64()
        .filter(|f| f.is_finite())
        .ok_or_else(|| CodecError::new(at, format!("number {n} is not a finite double")))
}

fn decode_tag(tag: &str, inner: &Json, at: &str) -> Result<Value, CodecError> {
    let string = || {
        inner
            .as_str()
            .ok_or_else(|| CodecError::new(at, format!("`{tag}` expects a string")))
    };
    match tag {
        "$ts" => string().and_then(|s| {
            s.parse::<Timestamp>()
                .map(Value::Timestamp)
                .map_err(|e| CodecError::new(at, format!("invalid RFC 3339 timestamp: {e}")))
        }),
        "$ref" => string().and_then(|s| {
            s.parse()
                .map(Value::Ref)
                .map_err(|e| CodecError::new(at, format!("invalid document path: {e}")))
        }),
        "$bytes" => string().and_then(|s| {
            STANDARD
                .decode(s)
                .map(Value::Bytes)
                .map_err(|e| CodecError::new(at, format!("invalid base64: {e}")))
        }),
        "$double" => decode_double(inner, at).map(Value::Double),
        "$geo" => decode_geo(inner, at),
        "$map" => inner
            .as_object()
            .ok_or_else(|| CodecError::new(at, "`$map` expects an object"))
            .and_then(|o| decode_fields(o, &child(at, "$map")))
            .map(Value::Map),
        other => Err(CodecError::new(
            at,
            format!("unknown tag `{other}` (known: $ts, $ref, $double, $bytes, $geo, $map)"),
        )),
    }
}

fn decode_double(inner: &Json, at: &str) -> Result<f64, CodecError> {
    match inner {
        Json::Number(n) => finite(n, at),
        Json::String(s) => match s.as_str() {
            "NaN" => Ok(f64::NAN),
            "Infinity" => Ok(f64::INFINITY),
            "-Infinity" => Ok(f64::NEG_INFINITY),
            other => Err(CodecError::new(
                at,
                format!("`$double` string must be NaN, Infinity or -Infinity, got `{other}`"),
            )),
        },
        _ => Err(CodecError::new(
            at,
            "`$double` expects a number or a string",
        )),
    }
}

fn decode_geo(inner: &Json, at: &str) -> Result<Value, CodecError> {
    let bad = || {
        CodecError::new(
            at,
            r#"`$geo` expects exactly {"lat": number, "lng": number}"#,
        )
    };
    let o = inner.as_object().filter(|o| o.len() == 2).ok_or_else(bad)?;
    let coord = |key: &str| match o.get(key) {
        Some(Json::Number(n)) => finite(n, at),
        _ => Err(bad()),
    };
    GeoPoint::new(coord("lat")?, coord("lng")?)
        .map(Value::Geo)
        .map_err(|e| CodecError::new(at, e.to_string()))
}
