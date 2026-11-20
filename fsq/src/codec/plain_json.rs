//! Untagged, one-way JSON for humans and `jq` (`--plain`); not meant to be read back.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value as Json, json};

use super::{double_json, fields_to_object};
use crate::domain::document::Document;
use crate::domain::value::Value;

/// Renders a value without type tags: timestamps, references and bytes (standard
/// base64) become strings, geo points `{"lat","lng"}`, and non-finite doubles
/// the strings `"NaN"`, `"Infinity"`, `"-Infinity"`.
#[must_use]
pub fn value_to_json(v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Int(i) => Json::from(*i),
        Value::Double(f) => double_json(*f),
        Value::String(s) => Json::String(s.clone()),
        Value::Timestamp(t) => Json::String(t.to_string()),
        Value::Bytes(b) => Json::String(STANDARD.encode(b)),
        Value::Ref(p) => Json::String(p.to_string()),
        Value::Geo(g) => json!({"lat": double_json(g.lat()), "lng": double_json(g.lng())}),
        Value::Array(xs) => Json::Array(xs.iter().map(value_to_json).collect()),
        Value::Map(m) => Json::Object(fields_to_object(m, value_to_json)),
    }
}

/// Renders a document as `{"_path", "_updateTime", "data"}` with untagged data.
#[must_use]
pub fn document_to_json(d: &Document) -> Json {
    json!({
        "_path": d.path.to_string(),
        "_updateTime": d.update_time.to_string(),
        "data": Json::Object(fields_to_object(&d.data, value_to_json)),
    })
}
