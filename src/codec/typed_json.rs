//! Typed JSON is lossless: what `get` prints is what `set --data` reads back. Format: README.
//!
//! - Integers never pass through `f64`. A number is an integer only if it is an `i64` literal;
//!   any integral number of magnitude `>= 2^63` is an error (without arbitrary precision
//!   `serde_json` cannot tell a huge integer from an exponent double), so write `{"$double": n}`.
//!   `-0` has no integer form and decodes as the double `-0.0`.
//! - Doubles are always tagged, so a double never reads back as an integer.
//! - An object with one `$` key must be a known tag: a typo must not silently become a map.
//!   A map whose only key starts with `$` is written `{"$map": {...}}` to keep the round trip exact.
//! - An array directly inside an array is rejected: Firestore forbids it.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Number, Value as Json, json};

use super::{CodecError, double_json, fields_to_object};
use crate::domain::document::Document;
use crate::domain::field::FieldPath;
use crate::domain::path::DocPath;
use crate::domain::value::{DocResourceName, Fields, GeoPoint, Timestamp, UpdateTime, Value};

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
        Value::ForeignRef(n) => json!({"$ref": n.as_str()}),
        Value::Geo(g) => {
            json!({"$geo": {"lat": double_json(g.lat()), "lng": double_json(g.lng())}})
        }
        Value::Array(xs) => Json::Array(xs.iter().map(value_to_json).collect()),
        Value::Map(m) => map_to_json(m),
    }
}

fn map_to_json(fields: &Fields) -> Json {
    let object = fields_to_object(fields, value_to_json);
    if object.len() == 1 && object.keys().all(|k| k.starts_with('$')) {
        json!({"$map": object})
    } else {
        Json::Object(object)
    }
}

/// `data` holds the fields unescaped, unlike a map value.
#[must_use]
pub fn document_to_json(d: &Document) -> Json {
    json!({
        "_path": d.path.to_string(),
        "_updateTime": d.update_time.to_string(),
        "data": Json::Object(fields_to_object(&d.data, value_to_json)),
    })
}

pub fn value_from_json(j: &Json) -> Result<Value, CodecError> {
    decode(j, "")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub fields: Fields,
    pub envelope: Option<Envelope>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub path: Option<DocPath>,
    pub update_time: Option<UpdateTime>,
    pub plain: bool,
    pub select: Option<Vec<FieldPath>>,
}

/// A *full document* only if it has `_path` or `_updateTime` (then `data` is required and any other
/// top-level key besides the `_plain`/`_select` markers is an error, not dropped); anything else
/// is bare data, so `{"data": {...}}` alone is a field named `data`. The markers are reserved
/// top-level keys: to store a field named so, use the full-document form and put it in `data`.
pub fn parse_input(j: &Json) -> Result<Input, CodecError> {
    let top = j
        .as_object()
        .ok_or_else(|| CodecError::new("", "expected a JSON object"))?;
    let is_full_document = ["_path", "_updateTime"]
        .iter()
        .any(|k| top.contains_key(*k));
    let plain = top.contains_key("_plain");
    let select = top.get("_select").map(select_paths).transpose()?;
    if !is_full_document && !plain && select.is_none() {
        return decode_fields(top, "").map(|fields| Input {
            fields,
            envelope: None,
        });
    }
    let fields = if is_full_document {
        full_document_fields(top)?
    } else {
        let data: Map<String, Json> = top
            .iter()
            .filter(|(k, _)| !matches!(k.as_str(), "_plain" | "_select"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        decode_fields(&data, "")?
    };
    let envelope = Envelope {
        path: envelope_value(top, "_path", |s| {
            s.parse::<DocPath>()
                .map_err(|e| format!("`{s}` is not a document path: {e}"))
        })?,
        update_time: envelope_value(top, "_updateTime", |s| {
            s.parse::<UpdateTime>()
                .map_err(|e| format!("invalid RFC 3339 timestamp: {e}"))
        })?,
        plain,
        select,
    };
    Ok(Input {
        fields,
        envelope: Some(envelope),
    })
}

fn full_document_fields(top: &Map<String, Json>) -> Result<Fields, CodecError> {
    if let Some(stray) = top.keys().find(|k| {
        !matches!(
            k.as_str(),
            "_path" | "_updateTime" | "data" | "_plain" | "_select"
        )
    }) {
        return Err(CodecError::new(
            &child("", stray),
            format!(
                "unexpected top-level key `{stray}` in a full document; put fields inside `data`"
            ),
        ));
    }
    top.get("data")
        .and_then(Json::as_object)
        .ok_or_else(|| CodecError::new("/data", "a full document needs a `data` object"))
        .and_then(|data| decode_fields(data, "/data"))
}

fn envelope_value<T>(
    top: &Map<String, Json>,
    key: &str,
    parse: impl Fn(&str) -> Result<T, String>,
) -> Result<Option<T>, CodecError> {
    let at = child("", key);
    top.get(key)
        .map(|v| {
            let s = v
                .as_str()
                .ok_or_else(|| CodecError::new(&at, "must be a string"))?;
            parse(s).map_err(|reason| CodecError::new(&at, reason))
        })
        .transpose()
}

fn select_paths(j: &Json) -> Result<Vec<FieldPath>, CodecError> {
    j.as_array()
        .ok_or_else(|| CodecError::new("/_select", "must be an array of field paths"))?
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let at = child("/_select", &i.to_string());
            let s = p
                .as_str()
                .ok_or_else(|| CodecError::new(&at, "must be a string"))?;
            s.parse::<FieldPath>()
                .map_err(|e| CodecError::new(&at, e.to_string()))
        })
        .collect()
}

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
            .map(|(i, x)| {
                let here = child(at, &i.to_string());
                if x.is_array() {
                    Err(CodecError::new(
                        &here,
                        "an array directly inside an array is not allowed by Firestore",
                    ))
                } else {
                    decode(x, &here)
                }
            })
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
    // 2^63 as f64: every integral f64 at or beyond it is outside the i64 range.
    const I64_LIMIT: f64 = 9_223_372_036_854_775_808.0;
    if let Some(i) = n.as_i64() {
        return Ok(Value::Int(i));
    }
    let out_of_range = || {
        CodecError::new(
            at,
            format!(
                "number {n} out of range for an integer; write {{\"$double\": n}} for a double"
            ),
        )
    };
    if n.is_u64() {
        return Err(out_of_range());
    }
    let f = finite(n, at)?;
    if f.fract() == 0.0 && f.abs() >= I64_LIMIT {
        Err(out_of_range())
    } else {
        Ok(Value::Double(f))
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
        "$ts" => {
            let s = string()?;
            s.parse::<Timestamp>()
                .map(Value::Timestamp)
                .map_err(|e| CodecError::new(at, format!("invalid RFC 3339 timestamp: {e}")))
        }
        "$ref" => {
            let s = string()?;
            decode_ref(s, at)
        }
        "$bytes" => {
            let s = string()?;
            STANDARD
                .decode(s)
                .map(Value::Bytes)
                .map_err(|e| CodecError::new(at, format!("invalid base64: {e}")))
        }
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
        Json::Null | Json::Bool(_) | Json::Array(_) | Json::Object(_) => Err(CodecError::new(
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

/// A resource name has an odd number of segments, a document path an even one, so the two never overlap.
fn decode_ref(s: &str, at: &str) -> Result<Value, CodecError> {
    if let Some(name) = s
        .starts_with("projects/")
        .then(|| s.parse::<DocResourceName>().ok())
        .flatten()
    {
        return Ok(Value::ForeignRef(name));
    }
    s.parse::<DocPath>()
        .map(Value::Ref)
        .map_err(|e| CodecError::new(at, format!("invalid document path: {e}")))
}
