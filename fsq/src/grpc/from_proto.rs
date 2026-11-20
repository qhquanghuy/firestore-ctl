//! Firestore protos to domain types. Pure; anything this program cannot represent
//! (pipeline-only values, a malformed reference) is an error, never a guess. The
//! one exception is [`index`]: listing indexes skips those the domain cannot express.

use std::collections::HashMap;

use gcloud_sdk::google::firestore::admin::v1 as admin;
use gcloud_sdk::google::firestore::admin::v1::index::index_field::{ArrayConfig, Order, ValueMode};
use gcloud_sdk::google::firestore::admin::v1::index::{QueryScope as ProtoScope, State};
use gcloud_sdk::google::firestore::v1 as proto;
use gcloud_sdk::google::firestore::v1::value::ValueType;
use gcloud_sdk::prost_types;

use crate::domain::{
    CollectionId, DbTarget, DocPath, Document, FieldPath, Fields, GeoPoint, IndexField, IndexMode,
    IndexSpec, NonEmptyVec, QueryScope, ResourceNameError, Timestamp, UpdateTime, Value,
};
use crate::port::{GatewayError, IndexInfo, IndexState};

fn other(message: impl Into<String>) -> GatewayError {
    GatewayError::Other(message.into())
}

/// The document path of resource `name`, which must lie under `t`'s documents root.
/// The root is matched with its trailing `/`, so `d2` is not mistaken for `d`.
fn doc_path(name: &str, t: &DbTarget) -> Result<DocPath, GatewayError> {
    name.strip_prefix(&format!("{}/", t.documents_root()))
        .ok_or_else(|| {
            other(format!(
                "`{name}` is not in database {}/{}",
                t.project, t.database
            ))
        })?
        .parse()
        .map_err(|e| other(format!("`{name}` is not a document: {e}")))
}

/// A document read from `t`.
///
/// # Errors
/// [`GatewayError::Other`] if the document is not in `t`, has no update time, or holds a
/// value this program cannot represent.
pub fn document(d: proto::Document, t: &DbTarget) -> Result<Document, GatewayError> {
    let path = doc_path(&d.name, t)?;
    let update_time = d
        .update_time
        .as_ref()
        .ok_or_else(|| other(format!("document {path} has no update time")))
        .and_then(update_time)?;
    let data = fields(d.fields, t)?;
    Ok(Document {
        path,
        update_time,
        data,
    })
}

fn fields(m: HashMap<String, proto::Value>, t: &DbTarget) -> Result<Fields, GatewayError> {
    m.into_iter()
        .map(|(k, v)| value(v, t).map(|v| (k, v)))
        .collect()
}

fn timestamp(ts: &prost_types::Timestamp) -> Result<Timestamp, GatewayError> {
    u32::try_from(ts.nanos)
        .ok()
        .and_then(|nanos| Timestamp::from_unix(ts.seconds, nanos))
        .ok_or_else(|| {
            other(format!(
                "timestamp {}s {}ns is out of range",
                ts.seconds, ts.nanos
            ))
        })
}

/// A value read from `t`. A reference into `t` is a relative [`Value::Ref`]; one into
/// another database or project is a [`Value::ForeignRef`] holding its full name.
///
/// # Errors
/// [`GatewayError::Other`] for a value that is absent, out of range, a malformed
/// reference, or of a pipeline-only kind.
pub fn value(v: proto::Value, t: &DbTarget) -> Result<Value, GatewayError> {
    match v.value_type {
        Some(ValueType::NullValue(_)) => Ok(Value::Null),
        Some(ValueType::BooleanValue(b)) => Ok(Value::Bool(b)),
        Some(ValueType::IntegerValue(i)) => Ok(Value::Int(i)),
        Some(ValueType::DoubleValue(d)) => Ok(Value::Double(d)),
        Some(ValueType::TimestampValue(ts)) => timestamp(&ts).map(Value::Timestamp),
        Some(ValueType::StringValue(s)) => Ok(Value::String(s)),
        Some(ValueType::BytesValue(b)) => Ok(Value::Bytes(b)),
        Some(ValueType::ReferenceValue(name)) => reference(&name, t),
        Some(ValueType::GeoPointValue(g)) => GeoPoint::new(g.latitude, g.longitude)
            .map(Value::Geo)
            .map_err(|e| other(format!("invalid geo point: {e}"))),
        Some(ValueType::ArrayValue(a)) => a
            .values
            .into_iter()
            .map(|v| value(v, t))
            .collect::<Result<_, _>>()
            .map(Value::Array),
        Some(ValueType::MapValue(m)) => fields(m.fields, t).map(Value::Map),
        Some(
            ValueType::FieldReferenceValue(_)
            | ValueType::VariableReferenceValue(_)
            | ValueType::FunctionValue(_)
            | ValueType::PipelineValue(_),
        ) => Err(other("the document holds a pipeline-only value type")),
        None => Err(other("the document holds a value with no type")),
    }
}

/// The reference `name`: relative when it lies in `t`, otherwise kept as a full name.
fn reference(name: &str, t: &DbTarget) -> Result<Value, GatewayError> {
    if name.starts_with(&format!("{}/", t.documents_root())) {
        doc_path(name, t).map(Value::Ref)
    } else {
        name.parse()
            .map(Value::ForeignRef)
            .map_err(|e: ResourceNameError| other(e.to_string()))
    }
}

/// A proto timestamp as an update time.
///
/// # Errors
/// [`GatewayError::Other`] if the timestamp is out of range.
pub fn update_time(t: &prost_types::Timestamp) -> Result<UpdateTime, GatewayError> {
    timestamp(t).map(UpdateTime::new)
}

/// An existing composite index of collection group `c`.
///
/// Indexes the domain cannot express are skipped (`None`), never approximated: vector or
/// search fields, recursive scope, an unknown field mode, a field path that does not
/// parse. Only the implicit trailing `__name__` field is dropped.
pub fn index(c: &CollectionId, i: &admin::Index) -> Option<IndexInfo> {
    let scope = match ProtoScope::try_from(i.query_scope).ok()? {
        ProtoScope::Collection => QueryScope::Collection,
        ProtoScope::CollectionGroup => QueryScope::CollectionGroup,
        ProtoScope::Unspecified | ProtoScope::CollectionRecursive => return None,
    };
    let explicit = match i.fields.split_last() {
        Some((last, rest)) if last.field_path == "__name__" => rest,
        _ => &i.fields[..],
    };
    let fields: Vec<IndexField> = explicit.iter().map(index_field).collect::<Option<_>>()?;
    Some(IndexInfo {
        spec: IndexSpec {
            collection_group: c.clone(),
            scope,
            fields: NonEmptyVec::try_from(fields).ok()?,
        },
        state: match State::try_from(i.state) {
            Ok(State::Ready) => IndexState::Ready,
            Ok(State::Creating) => IndexState::Creating,
            Ok(State::NeedsRepair) => IndexState::NeedsRepair,
            Ok(State::Unspecified) | Err(_) => IndexState::Unknown,
        },
    })
}

fn index_field(f: &admin::index::IndexField) -> Option<IndexField> {
    let mode = match f.value_mode.as_ref()? {
        ValueMode::Order(o) => match Order::try_from(*o).ok()? {
            Order::Ascending => IndexMode::Asc,
            Order::Descending => IndexMode::Desc,
            Order::Unspecified => return None,
        },
        ValueMode::ArrayConfig(a) => match ArrayConfig::try_from(*a).ok()? {
            ArrayConfig::Contains => IndexMode::ArrayContains,
            ArrayConfig::Unspecified => return None,
        },
        _ => return None,
    };
    let field = f.field_path.parse::<FieldPath>().ok()?;
    Some(IndexField { field, mode })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use gcloud_sdk::google::firestore::admin::v1::index::index_field::{
        ArrayConfig, Order, ValueMode,
    };
    use gcloud_sdk::google::firestore::admin::v1::index::{
        IndexField, QueryScope as ProtoScope, State,
    };
    use gcloud_sdk::google::firestore::v1::value::ValueType;
    use gcloud_sdk::prost_types;

    use super::*;
    use crate::domain::{GeoPoint, IndexMode, QueryScope};
    use crate::grpc::test_util::{doc, target};
    use crate::grpc::to_proto;
    use crate::port::IndexState;

    fn every_variant() -> Vec<Value> {
        let ts: Timestamp = "2026-01-05T10:00:00.123456789Z".parse().unwrap();
        vec![
            Value::Null,
            Value::Bool(true),
            Value::Int(-7),
            Value::Double(1.5),
            Value::String("héllo".to_owned()),
            Value::Timestamp(ts),
            Value::Bytes(vec![0, 1, 255]),
            Value::Ref(doc("users/u2/orders/o1")),
            Value::ForeignRef(
                "projects/q/databases/(default)/documents/users/u9"
                    .parse()
                    .unwrap(),
            ),
            Value::Geo(GeoPoint::new(35.5, 139.7).unwrap()),
            Value::Array(vec![Value::Int(1), Value::Array(vec![]), Value::Null]),
            Value::Map(BTreeMap::from([
                ("a".to_owned(), Value::Int(1)),
                ("b".to_owned(), Value::Map(BTreeMap::new())),
            ])),
        ]
    }

    #[test]
    fn value_roundtrip_through_proto() {
        let t = target("p", "jobs-db");
        for v in every_variant() {
            let back = value(to_proto::value(&v, &t), &t).unwrap();
            assert_eq!(back, v);
        }
    }

    #[test]
    fn references_outside_the_database_become_foreign_refs() {
        let ours = target("p", "d");
        for other_db in [target("p", "d2"), target("q", "d")] {
            let foreign = to_proto::value(&Value::Ref(doc("users/u1")), &other_db);
            let name = format!("{}/users/u1", other_db.documents_root());
            assert_eq!(
                value(foreign, &ours),
                Ok(Value::ForeignRef(name.parse().unwrap()))
            );
        }
        // a sibling database whose name merely starts with ours is still foreign
        let sibling = to_proto::value(&Value::Ref(doc("a/b")), &target("p", "d-2"));
        assert!(matches!(value(sibling, &ours), Ok(Value::ForeignRef(_))));
    }

    #[test]
    fn foreign_refs_roundtrip_and_are_written_verbatim() {
        let t = target("p", "d");
        let name = "projects/q/databases/(default)/documents/users/u1/orders/o1";
        let v = Value::ForeignRef(name.parse().unwrap());
        let proto = to_proto::value(&v, &t);
        assert_eq!(
            proto.value_type,
            Some(ValueType::ReferenceValue(name.to_owned()))
        );
        assert_eq!(value(proto, &t), Ok(v));
    }

    #[test]
    fn a_malformed_reference_is_an_error() {
        let t = target("p", "d");
        for name in [
            "users/u1",
            "projects/q/databases/d",
            "projects/q/databases/d/documents/users",
        ] {
            let bad = proto::Value {
                value_type: Some(ValueType::ReferenceValue(name.to_owned())),
            };
            assert!(
                matches!(value(bad, &t), Err(GatewayError::Other(_))),
                "{name}"
            );
        }
    }

    #[test]
    fn value_without_a_type_or_with_pipeline_kind_is_an_error() {
        let t = target("p", "d");
        let none = proto::Value { value_type: None };
        let pipeline = proto::Value {
            value_type: Some(ValueType::FieldReferenceValue("a".to_owned())),
        };
        assert!(value(none, &t).is_err());
        assert!(value(pipeline, &t).is_err());
    }

    #[test]
    fn out_of_range_geo_point_is_an_error() {
        let t = target("p", "d");
        let bad = proto::Value {
            value_type: Some(ValueType::GeoPointValue(
                gcloud_sdk::google::r#type::LatLng {
                    latitude: 91.0,
                    longitude: 0.0,
                },
            )),
        };
        assert!(value(bad, &t).is_err());
    }

    #[test]
    fn document_reads_path_time_and_fields() {
        let t = target("p", "d");
        let d = proto::Document {
            name: "projects/p/databases/d/documents/users/u1".to_owned(),
            fields: HashMap::from([("n".to_owned(), to_proto::value(&Value::Int(1), &t))]),
            create_time: None,
            update_time: Some(prost_types::Timestamp {
                seconds: 1_767_607_200,
                nanos: 123_456_789,
            }),
        };
        let got = document(d, &t).unwrap();
        assert_eq!(got.path, doc("users/u1"));
        assert_eq!(
            got.update_time.to_string(),
            "2026-01-05T10:00:00.123456789Z"
        );
        assert_eq!(got.data, BTreeMap::from([("n".to_owned(), Value::Int(1))]));
    }

    #[test]
    fn document_from_another_database_or_without_time_is_an_error() {
        let t = target("p", "d");
        let named = |name: &str, time: bool| proto::Document {
            name: name.to_owned(),
            fields: HashMap::new(),
            create_time: None,
            update_time: time.then_some(prost_types::Timestamp {
                seconds: 1,
                nanos: 0,
            }),
        };
        // `d2` must not match by prefix.
        assert!(
            document(
                named("projects/p/databases/d2/documents/users/u1", true),
                &t
            )
            .is_err()
        );
        assert!(
            document(
                named("projects/p/databases/d/documents/users/u1", false),
                &t
            )
            .is_err()
        );
        assert!(document(named("projects/p/databases/d/documents/users", true), &t).is_err());
    }

    fn index_field(path: &str, mode: ValueMode) -> IndexField {
        IndexField {
            field_path: path.to_owned(),
            value_mode: Some(mode),
        }
    }

    fn proto_index(fields: Vec<IndexField>, scope: ProtoScope, state: State) -> admin::Index {
        admin::Index {
            query_scope: scope as i32,
            fields,
            state: state as i32,
            ..Default::default()
        }
    }

    #[test]
    fn index_maps_fields_scope_and_state_and_drops_name_field() {
        let c: CollectionId = "orders".parse().unwrap();
        let i = proto_index(
            vec![
                index_field("status", ValueMode::Order(Order::Ascending as i32)),
                index_field("tags", ValueMode::ArrayConfig(ArrayConfig::Contains as i32)),
                index_field("created", ValueMode::Order(Order::Descending as i32)),
                index_field("__name__", ValueMode::Order(Order::Descending as i32)),
            ],
            ProtoScope::CollectionGroup,
            State::Creating,
        );
        let info = index(&c, &i).unwrap();
        assert_eq!(info.state, IndexState::Creating);
        assert_eq!(info.spec.scope, QueryScope::CollectionGroup);
        assert_eq!(info.spec.collection_group, c);
        let fields: Vec<_> = info
            .spec
            .fields
            .iter()
            .map(|f| (f.field.to_string(), f.mode))
            .collect();
        assert_eq!(
            fields,
            [
                ("status".to_owned(), IndexMode::Asc),
                ("tags".to_owned(), IndexMode::ArrayContains),
                ("created".to_owned(), IndexMode::Desc),
            ]
        );
    }

    #[test]
    fn index_states_map_and_unrepresentable_indexes_are_skipped() {
        let c: CollectionId = "orders".parse().unwrap();
        let ok = |state| {
            index(
                &c,
                &proto_index(
                    vec![index_field("a", ValueMode::Order(Order::Ascending as i32))],
                    ProtoScope::Collection,
                    state,
                ),
            )
            .map(|i| i.state)
        };
        assert_eq!(ok(State::Ready), Some(IndexState::Ready));
        assert_eq!(ok(State::NeedsRepair), Some(IndexState::NeedsRepair));
        assert_eq!(ok(State::Unspecified), Some(IndexState::Unknown));

        let recursive = proto_index(
            vec![index_field("a", ValueMode::Order(Order::Ascending as i32))],
            ProtoScope::CollectionRecursive,
            State::Ready,
        );
        assert_eq!(index(&c, &recursive), None);
        let name_only = proto_index(
            vec![index_field(
                "__name__",
                ValueMode::Order(Order::Ascending as i32),
            )],
            ProtoScope::Collection,
            State::Ready,
        );
        assert_eq!(index(&c, &name_only), None);
        let no_mode = proto_index(
            vec![IndexField {
                field_path: "a".to_owned(),
                value_mode: None,
            }],
            ProtoScope::Collection,
            State::Ready,
        );
        assert_eq!(index(&c, &no_mode), None);
    }

    #[test]
    fn index_with_an_unparsable_field_path_is_skipped_not_repaired() {
        let c: CollectionId = "orders".parse().unwrap();
        let bad = proto_index(
            vec![
                index_field("ok", ValueMode::Order(Order::Ascending as i32)),
                index_field("a`b", ValueMode::Order(Order::Ascending as i32)),
            ],
            ProtoScope::Collection,
            State::Ready,
        );
        assert_eq!(index(&c, &bad), None);
    }

    #[test]
    fn only_a_trailing_name_field_is_dropped() {
        let c: CollectionId = "orders".parse().unwrap();
        let asc = || ValueMode::Order(Order::Ascending as i32);
        let info = |fields| {
            index(
                &c,
                &proto_index(fields, ProtoScope::Collection, State::Ready),
            )
        };
        let paths = |i: IndexInfo| {
            i.spec
                .fields
                .iter()
                .map(|f| f.field.to_string())
                .collect::<Vec<_>>()
        };
        let middle = info(vec![
            index_field("a", asc()),
            index_field("__name__", asc()),
            index_field("b", asc()),
        ]);
        assert_eq!(paths(middle.unwrap()), ["a", "__name__", "b"]);
        let trailing = info(vec![
            index_field("a", asc()),
            index_field("__name__", asc()),
        ]);
        assert_eq!(paths(trailing.unwrap()), ["a"]);
    }
}
