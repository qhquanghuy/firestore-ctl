//! Every resource name is built from the [`DbTarget`] so no request can fall back to `(default)`.

use std::collections::HashMap;

use gcloud_sdk::google::firestore::v1 as proto;
use gcloud_sdk::google::firestore::v1::document_transform::FieldTransform;
use gcloud_sdk::google::firestore::v1::document_transform::field_transform::TransformType;
use gcloud_sdk::google::firestore::v1::precondition::ConditionType;
use gcloud_sdk::google::firestore::v1::structured_aggregation_query::{
    Aggregation as ProtoAggregation, aggregation,
};
use gcloud_sdk::google::firestore::v1::structured_query::filter::FilterType;
use gcloud_sdk::google::firestore::v1::structured_query::{
    CollectionSelector, CompositeFilter, Direction as ProtoDirection, FieldFilter, FieldReference,
    Filter as ProtoFilter, Order as ProtoOrder, Projection, UnaryFilter, composite_filter,
    field_filter, unary_filter,
};
use gcloud_sdk::google::firestore::v1::value::ValueType;
use gcloud_sdk::prost_types;

use crate::domain::{
    AggregateQuery, Aggregation, DbTarget, Direction, DocPath, FieldOp, FieldPath, Fields, Filter,
    Guard, Op, OrderBy, Query, Scope, SetDoc, SetMode, Timestamp, UpdateDoc, UpdateTime, Value,
    Write, check_field_ops,
};
use crate::port::GatewayError;

pub(super) fn value(v: &Value, t: &DbTarget) -> proto::Value {
    let value_type = match v {
        Value::Null => ValueType::NullValue(prost_types::NullValue::NullValue as i32),
        Value::Bool(b) => ValueType::BooleanValue(*b),
        Value::Int(i) => ValueType::IntegerValue(*i),
        Value::Double(d) => ValueType::DoubleValue(*d),
        Value::String(s) => ValueType::StringValue(s.clone()),
        Value::Timestamp(ts) => ValueType::TimestampValue(timestamp(*ts)),
        Value::Bytes(b) => ValueType::BytesValue(b.clone()),
        Value::Ref(p) => ValueType::ReferenceValue(doc_name(p, t)),
        Value::ForeignRef(name) => ValueType::ReferenceValue(name.as_str().to_owned()),
        Value::Geo(g) => ValueType::GeoPointValue(gcloud_sdk::google::r#type::LatLng {
            latitude: g.lat(),
            longitude: g.lng(),
        }),
        Value::Array(items) => ValueType::ArrayValue(array(items, t)),
        Value::Map(m) => ValueType::MapValue(proto::MapValue {
            fields: fields(m, t),
        }),
    };
    proto::Value {
        value_type: Some(value_type),
    }
}

fn array(items: &[Value], t: &DbTarget) -> proto::ArrayValue {
    proto::ArrayValue {
        values: items.iter().map(|v| value(v, t)).collect(),
    }
}

fn fields(m: &Fields, t: &DbTarget) -> HashMap<String, proto::Value> {
    m.iter().map(|(k, v)| (k.clone(), value(v, t))).collect()
}

fn timestamp(ts: Timestamp) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: ts.seconds(),
        // Below 2_000_000_000 even for a leap second, so it always fits.
        nanos: i32::try_from(ts.nanos()).unwrap_or(i32::MAX),
    }
}

pub(super) fn update_time(u: &UpdateTime) -> prost_types::Timestamp {
    timestamp(u.timestamp())
}

pub(super) fn doc_name(p: &DocPath, t: &DbTarget) -> String {
    format!("{}/{p}", t.documents_root())
}

pub(super) fn parent_name(parent: Option<&DocPath>, t: &DbTarget) -> String {
    parent.map_or_else(|| t.documents_root(), |p| doc_name(p, t))
}

fn field_ref(f: &FieldPath) -> FieldReference {
    FieldReference {
        field_path: f.to_firestore(),
    }
}

fn clamp(n: u32) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

pub(super) fn structured_query(q: &Query, t: &DbTarget) -> (String, proto::StructuredQuery) {
    let (parent, from) = match &q.scope {
        Scope::Collection(c) => (
            parent_name(c.parent(), t),
            CollectionSelector {
                collection_id: c.id().to_string(),
                all_descendants: false,
            },
        ),
        Scope::Group { parent, id } => (
            parent_name(parent.as_ref(), t),
            CollectionSelector {
                collection_id: id.to_string(),
                all_descendants: true,
            },
        ),
    };
    let query = proto::StructuredQuery {
        select: q.select.as_ref().map(|fs| Projection {
            fields: fs.iter().map(field_ref).collect(),
        }),
        from: vec![from],
        r#where: where_clause(&q.filters, t),
        order_by: q.order.iter().map(order).collect(),
        offset: clamp(q.offset),
        limit: q.limit.map(|l| clamp(l.get())),
        ..Default::default()
    };
    (parent, query)
}

fn order(o: &OrderBy) -> ProtoOrder {
    ProtoOrder {
        field: Some(field_ref(&o.field)),
        direction: match o.direction {
            Direction::Asc => ProtoDirection::Ascending,
            Direction::Desc => ProtoDirection::Descending,
        } as i32,
    }
}

fn where_clause(filters: &[Filter], t: &DbTarget) -> Option<ProtoFilter> {
    match filters {
        [] => None,
        [one] => Some(filter(one, t)),
        many => Some(ProtoFilter {
            filter_type: Some(FilterType::CompositeFilter(CompositeFilter {
                op: composite_filter::Operator::And as i32,
                filters: many.iter().map(|f| filter(f, t)).collect(),
            })),
        }),
    }
}

fn filter(f: &Filter, t: &DbTarget) -> ProtoFilter {
    let field = field_ref(f.field());
    let filter_type = match (f.op(), f.value()) {
        // Like the official SDKs: equality with null or NaN is a unary filter.
        (Op::Eq, Value::Null) => unary(field, unary_filter::Operator::IsNull),
        (Op::Ne, Value::Null) => unary(field, unary_filter::Operator::IsNotNull),
        (Op::Eq, Value::Double(d)) if d.is_nan() => unary(field, unary_filter::Operator::IsNan),
        (Op::Ne, Value::Double(d)) if d.is_nan() => unary(field, unary_filter::Operator::IsNotNan),
        (op, v) => FilterType::FieldFilter(FieldFilter {
            field: Some(field),
            op: field_operator(op) as i32,
            value: Some(value(v, t)),
        }),
    };
    ProtoFilter {
        filter_type: Some(filter_type),
    }
}

fn unary(field: FieldReference, op: unary_filter::Operator) -> FilterType {
    FilterType::UnaryFilter(UnaryFilter {
        op: op as i32,
        operand_type: Some(unary_filter::OperandType::Field(field)),
    })
}

fn field_operator(op: Op) -> field_filter::Operator {
    match op {
        Op::Eq => field_filter::Operator::Equal,
        Op::Ne => field_filter::Operator::NotEqual,
        Op::Lt => field_filter::Operator::LessThan,
        Op::Le => field_filter::Operator::LessThanOrEqual,
        Op::Gt => field_filter::Operator::GreaterThan,
        Op::Ge => field_filter::Operator::GreaterThanOrEqual,
        Op::In => field_filter::Operator::In,
        Op::NotIn => field_filter::Operator::NotIn,
        Op::ArrayContains => field_filter::Operator::ArrayContains,
        Op::ArrayContainsAny => field_filter::Operator::ArrayContainsAny,
    }
}

pub(super) fn aggregation_alias(i: usize) -> String {
    format!("agg_{i}")
}

pub(super) fn aggregation_query(
    q: &AggregateQuery,
    t: &DbTarget,
) -> (String, proto::StructuredAggregationQuery) {
    let (parent, query) = structured_query(&q.query, t);
    let aggregations = q
        .aggregations
        .iter()
        .enumerate()
        .map(|(i, a)| ProtoAggregation {
            alias: aggregation_alias(i),
            operator: Some(match a {
                Aggregation::Count => aggregation::Operator::Count(aggregation::Count::default()),
                Aggregation::Sum(f) => aggregation::Operator::Sum(aggregation::Sum {
                    field: Some(field_ref(f)),
                }),
                Aggregation::Avg(f) => aggregation::Operator::Avg(aggregation::Avg {
                    field: Some(field_ref(f)),
                }),
            }),
        })
        .collect();
    (
        parent,
        proto::StructuredAggregationQuery {
            aggregations,
            query_type: Some(
                proto::structured_aggregation_query::QueryType::StructuredQuery(query),
            ),
        },
    )
}

fn precondition(c: ConditionType) -> proto::Precondition {
    proto::Precondition {
        condition_type: Some(c),
    }
}

/// A guard replaces the existence check (Firestore cannot combine the two);
/// without one, `default` applies.
fn guarded(guard: Guard, default: Option<ConditionType>) -> Option<proto::Precondition> {
    match guard {
        Guard::UnchangedSince(u) => Some(precondition(ConditionType::UpdateTime(update_time(&u)))),
        Guard::None => default.map(precondition),
    }
}

pub(super) fn write(w: &Write, t: &DbTarget) -> Result<proto::Write, GatewayError> {
    match w {
        Write::Set(s) => set(s, t),
        Write::Update(u) => update(u, t),
        Write::Delete(d) => Ok(proto::Write {
            operation: Some(proto::write::Operation::Delete(doc_name(&d.path, t))),
            current_document: guarded(d.guard, Some(ConditionType::Exists(true))),
            ..Default::default()
        }),
    }
}

fn set(s: &SetDoc, t: &DbTarget) -> Result<proto::Write, GatewayError> {
    let (current_document, update_mask) = match s.mode {
        SetMode::Create => (Some(precondition(ConditionType::Exists(false))), None),
        SetMode::Overwrite(g) => (guarded(g, None), None),
        SetMode::Merge(g) => (guarded(g, None), Some(merge_mask(&s.data)?)),
    };
    Ok(proto::Write {
        operation: Some(proto::write::Operation::Update(proto::Document {
            name: doc_name(&s.path, t),
            fields: fields(&s.data, t),
            ..Default::default()
        })),
        update_mask,
        current_document,
        ..Default::default()
    })
}

/// Always a mask, even an empty one: a missing mask would replace the document.
fn merge_mask(data: &Fields) -> Result<proto::DocumentMask, GatewayError> {
    fn leaves(prefix: &[String], data: &Fields) -> Vec<Vec<String>> {
        data.iter()
            .flat_map(|(key, v)| {
                let path: Vec<String> = prefix.iter().cloned().chain([key.clone()]).collect();
                if let Value::Map(m) = v
                    && !m.is_empty()
                {
                    leaves(&path, m)
                } else {
                    vec![path]
                }
            })
            .collect()
    }
    leaves(&[], data)
        .into_iter()
        .map(|segments| {
            FieldPath::from_segments(segments)
                .map(|p| p.to_firestore())
                .map_err(|e| GatewayError::Other(format!("cannot merge this document: {e}")))
        })
        .collect::<Result<_, _>>()
        .map(|field_paths| proto::DocumentMask { field_paths })
}

fn update(u: &UpdateDoc, t: &DbTarget) -> Result<proto::Write, GatewayError> {
    check_field_ops(&u.ops).map_err(|e| GatewayError::Other(e.to_string()))?;
    let sets = u.ops.iter().filter_map(|op| match op {
        FieldOp::Set(path, v) => Some((path, value(v, t))),
        FieldOp::ArrayUnion(..) | FieldOp::ArrayRemove(..) => None,
    });
    let (mask, nested) = sets.fold(
        (Vec::new(), HashMap::new()),
        |(mut mask, mut nested), (path, v)| {
            mask.push(path.to_firestore());
            insert_nested(&mut nested, path.segments(), v);
            (mask, nested)
        },
    );
    Ok(proto::Write {
        operation: Some(proto::write::Operation::Update(proto::Document {
            name: doc_name(&u.path, t),
            fields: nested,
            ..Default::default()
        })),
        update_mask: Some(proto::DocumentMask { field_paths: mask }),
        update_transforms: u
            .ops
            .iter()
            .filter_map(|op| array_transform(op, t))
            .collect(),
        current_document: guarded(u.guard, Some(ConditionType::Exists(true))),
    })
}

fn array_transform(op: &FieldOp, t: &DbTarget) -> Option<FieldTransform> {
    let (path, transform) = match op {
        FieldOp::Set(..) => return None,
        FieldOp::ArrayUnion(p, items) => (p, TransformType::AppendMissingElements(array(items, t))),
        FieldOp::ArrayRemove(p, items) => (p, TransformType::RemoveAllFromArray(array(items, t))),
    };
    Some(FieldTransform {
        field_path: path.to_firestore(),
        transform_type: Some(transform),
    })
}

fn insert_nested(fields: &mut HashMap<String, proto::Value>, segments: &[String], v: proto::Value) {
    match segments {
        [] => {}
        [last] => {
            fields.insert(last.clone(), v);
        }
        [head, rest @ ..] => {
            let child = fields.entry(head.clone()).or_default();
            let mut map = match child.value_type.take() {
                Some(ValueType::MapValue(m)) => m.fields,
                _ => HashMap::new(),
            };
            insert_nested(&mut map, rest, v);
            child.value_type = Some(ValueType::MapValue(proto::MapValue { fields: map }));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use gcloud_sdk::google::firestore::v1::document_transform::field_transform::TransformType;
    use gcloud_sdk::google::firestore::v1::precondition::ConditionType;
    use gcloud_sdk::google::firestore::v1::structured_aggregation_query::aggregation::Operator as AggOp;
    use gcloud_sdk::google::firestore::v1::structured_query::filter::FilterType;
    use gcloud_sdk::google::firestore::v1::structured_query::{field_filter, unary_filter};
    use gcloud_sdk::google::firestore::v1::value::ValueType;
    use gcloud_sdk::google::firestore::v1::write::Operation;

    use super::*;
    use crate::domain::{
        AggregateQuery, Aggregation, DeleteDoc, FieldOp, Guard, Limit, NonEmptyVec, Op, SetDoc,
        SetMode, UpdateDoc,
    };
    use crate::grpc::test_util::{doc, eq, query, target};

    const ROOT: &str = "projects/p/databases/jobs-db/documents";

    fn named() -> DbTarget {
        target("p", "jobs-db")
    }

    fn at(s: &str) -> UpdateTime {
        s.parse().unwrap()
    }

    fn precondition(w: &proto::Write) -> Option<ConditionType> {
        let p = w.current_document?;
        p.condition_type
    }

    fn set(mode: SetMode, data: BTreeMap<String, Value>) -> Write {
        Write::Set(SetDoc {
            path: doc("users/u1"),
            data,
            mode,
        })
    }

    fn delete(guard: Guard) -> Write {
        Write::Delete(DeleteDoc {
            path: doc("users/u1"),
            guard,
        })
    }

    fn update(ops: Vec<FieldOp>, guard: Guard) -> Write {
        Write::Update(UpdateDoc {
            path: doc("users/u1"),
            ops: NonEmptyVec::try_from(ops).unwrap(),
            guard,
        })
    }

    fn field_path(s: &str) -> FieldPath {
        s.parse().unwrap()
    }

    fn mask(w: &proto::Write) -> Option<Vec<String>> {
        w.update_mask.as_ref().map(|m| m.field_paths.clone())
    }

    #[test]
    fn request_parent_uses_named_database() {
        let (parent, _) = structured_query(&query("users/a/orders"), &named());
        assert_eq!(parent, format!("{ROOT}/users/a"));
    }

    #[test]
    fn top_level_collection_parent_is_the_documents_root() {
        let (parent, q) = structured_query(&query("users"), &named());
        assert_eq!(parent, ROOT);
        assert_eq!(q.from.len(), 1);
        assert_eq!(q.from[0].collection_id, "users");
        assert!(!q.from[0].all_descendants);
    }

    #[test]
    fn group_query_sets_all_descendants() {
        use crate::domain::Scope;
        let mut q = query("users");
        q.scope = Scope::Group {
            parent: None,
            id: "orders".parse().unwrap(),
        };
        let (parent, sq) = structured_query(&q, &named());
        assert_eq!(parent, ROOT);
        assert_eq!(sq.from[0].collection_id, "orders");
        assert!(sq.from[0].all_descendants);

        q.scope = Scope::Group {
            parent: Some(doc("users/a")),
            id: "orders".parse().unwrap(),
        };
        let (parent, _) = structured_query(&q, &named());
        assert_eq!(parent, format!("{ROOT}/users/a"));
    }

    #[test]
    fn field_paths_are_quoted_in_proto() {
        let mut q = query("users");
        q.filters = vec![eq("my-field", Value::from("x"))];
        let (_, sq) = structured_query(&q, &named());
        let Some(FilterType::FieldFilter(f)) = sq.r#where.and_then(|w| w.filter_type) else {
            panic!("expected a field filter");
        };
        assert_eq!(f.field.unwrap().field_path, "`my-field`");
        assert_eq!(f.op, field_filter::Operator::Equal as i32);
    }

    #[test]
    fn several_filters_are_anded_and_one_is_not_wrapped() {
        let mut q = query("users");
        q.filters = vec![eq("a", Value::Int(1))];
        let (_, one) = structured_query(&q, &named());
        assert!(matches!(
            one.r#where.and_then(|w| w.filter_type),
            Some(FilterType::FieldFilter(_))
        ));
        q.filters = vec![eq("a", Value::Int(1)), eq("b", Value::Int(2))];
        let (_, two) = structured_query(&q, &named());
        let Some(FilterType::CompositeFilter(c)) = two.r#where.and_then(|w| w.filter_type) else {
            panic!("expected a composite filter");
        };
        assert_eq!(c.filters.len(), 2);
        assert_eq!(c.op, composite_filter::Operator::And as i32);
    }

    #[test]
    fn equality_with_null_and_nan_uses_unary_filters() {
        let unary = |op, value| {
            let mut q = query("users");
            q.filters = vec![Filter::new(field_path("a"), op, value).unwrap()];
            let (_, sq) = structured_query(&q, &named());
            let Some(FilterType::UnaryFilter(u)) = sq.r#where.and_then(|w| w.filter_type) else {
                panic!("expected a unary filter");
            };
            u.op
        };
        assert_eq!(
            unary(Op::Eq, Value::Null),
            unary_filter::Operator::IsNull as i32
        );
        assert_eq!(
            unary(Op::Ne, Value::Null),
            unary_filter::Operator::IsNotNull as i32
        );
        assert_eq!(
            unary(Op::Eq, Value::Double(f64::NAN)),
            unary_filter::Operator::IsNan as i32
        );
        assert_eq!(
            unary(Op::Ne, Value::Double(f64::NAN)),
            unary_filter::Operator::IsNotNan as i32
        );
    }

    #[test]
    fn limit_offset_select_and_order_are_carried() {
        use crate::domain::{Direction, OrderBy};
        let mut q = query("users");
        q.limit = Some(Limit::from(std::num::NonZeroU32::new(3).unwrap()));
        q.offset = 2;
        q.select = Some(vec![field_path("a"), field_path("b.c")]);
        q.order = vec![OrderBy {
            field: field_path("a"),
            direction: Direction::Desc,
        }];
        let (_, sq) = structured_query(&q, &named());
        assert_eq!(sq.limit, Some(3));
        assert_eq!(sq.offset, 2);
        assert_eq!(
            sq.select
                .unwrap()
                .fields
                .iter()
                .map(|f| f.field_path.as_str())
                .collect::<Vec<_>>(),
            ["a", "b.c"]
        );
        assert_eq!(sq.order_by.len(), 1);
        assert_eq!(
            sq.order_by[0].direction,
            proto::structured_query::Direction::Descending as i32
        );
        let (_, plain) = structured_query(&query("users"), &named());
        assert_eq!((plain.limit, plain.select), (None, None));
    }

    #[test]
    fn aggregations_get_positional_aliases() {
        let agg = AggregateQuery {
            query: query("users/a/orders"),
            aggregations: NonEmptyVec::try_from(vec![
                Aggregation::Count,
                Aggregation::Sum(field_path("total")),
                Aggregation::Avg(field_path("total")),
            ])
            .unwrap(),
        };
        let (parent, a) = aggregation_query(&agg, &named());
        assert_eq!(parent, format!("{ROOT}/users/a"));
        assert_eq!(
            a.aggregations
                .iter()
                .map(|x| x.alias.as_str())
                .collect::<Vec<_>>(),
            ["agg_0", "agg_1", "agg_2"]
        );
        assert!(matches!(a.aggregations[0].operator, Some(AggOp::Count(_))));
        assert!(matches!(a.aggregations[1].operator, Some(AggOp::Sum(_))));
        assert!(matches!(a.aggregations[2].operator, Some(AggOp::Avg(_))));
        assert!(a.query_type.is_some());
    }

    #[test]
    fn reference_values_use_the_full_resource_name() {
        let v = value(&Value::Ref(doc("users/u2")), &named());
        assert_eq!(
            v.value_type,
            Some(ValueType::ReferenceValue(format!("{ROOT}/users/u2")))
        );
    }

    #[test]
    fn update_time_to_proto_keeps_nanos() {
        let ts = update_time(&at("2026-01-05T10:00:00.123456789Z"));
        assert_eq!(ts.nanos, 123_456_789);
        assert_eq!(ts.seconds, 1_767_607_200);
    }

    #[test]
    fn create_sets_exists_false_precondition() {
        let w = write(&set(SetMode::Create, BTreeMap::new()), &named()).unwrap();
        assert_eq!(precondition(&w), Some(ConditionType::Exists(false)));
        let Some(Operation::Update(d)) = w.operation else {
            panic!("expected an update");
        };
        assert_eq!(d.name, format!("{ROOT}/users/u1"));
        assert_eq!(w.update_mask, None);
    }

    #[test]
    fn overwrite_and_merge_without_guard_have_no_precondition() {
        let overwrite = write(
            &set(SetMode::Overwrite(Guard::None), BTreeMap::new()),
            &named(),
        );
        let merge = write(&set(SetMode::Merge(Guard::None), BTreeMap::new()), &named());
        assert_eq!(precondition(&overwrite.unwrap()), None);
        assert_eq!(precondition(&merge.unwrap()), None);
    }

    #[test]
    fn delete_without_guard_requires_exists() {
        let w = write(&delete(Guard::None), &named()).unwrap();
        assert_eq!(precondition(&w), Some(ConditionType::Exists(true)));
        assert_eq!(
            w.operation,
            Some(Operation::Delete(format!("{ROOT}/users/u1")))
        );
    }

    #[test]
    fn guard_uses_update_time() {
        let t = at("2026-01-05T10:00:00.123456789Z");
        let expected = Some(ConditionType::UpdateTime(update_time(&t)));
        let guard = Guard::UnchangedSince(t);
        let writes = [
            delete(guard),
            set(SetMode::Overwrite(guard), BTreeMap::new()),
            set(SetMode::Merge(guard), BTreeMap::new()),
            update(vec![FieldOp::Set(field_path("a"), Value::Int(1))], guard),
        ];
        for w in &writes {
            assert_eq!(precondition(&write(w, &named()).unwrap()), expected);
        }
    }

    #[test]
    fn merge_masks_every_leaf_and_quotes_dotted_keys() {
        let data = BTreeMap::from([
            (
                "a".to_owned(),
                Value::Map(BTreeMap::from([
                    ("b".to_owned(), Value::Int(1)),
                    ("c.d".to_owned(), Value::Int(2)),
                ])),
            ),
            ("empty".to_owned(), Value::Map(BTreeMap::new())),
            ("list".to_owned(), Value::Array(vec![Value::Int(1)])),
            ("my-field".to_owned(), Value::Null),
        ]);
        let w = write(&set(SetMode::Merge(Guard::None), data), &named()).unwrap();
        assert_eq!(
            mask(&w),
            Some(vec![
                "a.b".to_owned(),
                "a.`c.d`".to_owned(),
                "empty".to_owned(),
                "list".to_owned(),
                "`my-field`".to_owned(),
            ])
        );
    }

    #[test]
    fn merge_with_no_data_still_sends_an_empty_mask() {
        let w = write(&set(SetMode::Merge(Guard::None), BTreeMap::new()), &named()).unwrap();
        assert_eq!(mask(&w), Some(Vec::new()));
    }

    #[test]
    fn merge_rejects_empty_field_names() {
        let data = BTreeMap::from([(String::new(), Value::Int(1))]);
        assert!(matches!(
            write(&set(SetMode::Merge(Guard::None), data), &named()),
            Err(GatewayError::Other(_))
        ));
    }

    #[test]
    fn update_sets_become_one_nested_document_and_mask() {
        let w = write(
            &update(
                vec![
                    FieldOp::Set(field_path("a.b"), Value::Int(1)),
                    FieldOp::Set(field_path("a.c"), Value::Int(2)),
                    FieldOp::Set(field_path("my-field"), Value::Null),
                ],
                Guard::None,
            ),
            &named(),
        )
        .unwrap();
        assert_eq!(
            mask(&w),
            Some(vec![
                "a.b".to_owned(),
                "a.c".to_owned(),
                "`my-field`".to_owned()
            ])
        );
        assert_eq!(precondition(&w), Some(ConditionType::Exists(true)));
        let Some(Operation::Update(d)) = w.operation else {
            panic!("expected an update");
        };
        let Some(ValueType::MapValue(a)) = d.fields["a"].value_type.clone() else {
            panic!("a should be a map");
        };
        let mut keys: Vec<_> = a.fields.keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["b", "c"]);
        assert!(d.fields.contains_key("my-field"));
    }

    #[test]
    fn update_array_union_becomes_transform() {
        let w = write(
            &update(
                vec![
                    FieldOp::ArrayUnion(field_path("tags"), vec![Value::from("x")]),
                    FieldOp::ArrayRemove(field_path("old"), vec![Value::Int(1)]),
                ],
                Guard::None,
            ),
            &named(),
        )
        .unwrap();
        assert_eq!(w.update_transforms.len(), 2);
        assert_eq!(w.update_transforms[0].field_path, "tags");
        assert!(matches!(
            w.update_transforms[0].transform_type,
            Some(TransformType::AppendMissingElements(_))
        ));
        assert_eq!(w.update_transforms[1].field_path, "old");
        assert!(matches!(
            w.update_transforms[1].transform_type,
            Some(TransformType::RemoveAllFromArray(_))
        ));
    }

    #[test]
    fn transform_only_update_still_sends_an_empty_mask() {
        let w = write(
            &update(
                vec![FieldOp::ArrayUnion(
                    field_path("tags"),
                    vec![Value::from("x")],
                )],
                Guard::None,
            ),
            &named(),
        )
        .unwrap();
        assert_eq!(mask(&w), Some(Vec::new()));
    }

    #[test]
    fn overlapping_update_paths_are_rejected_before_sending() {
        let set = |p: &str| FieldOp::Set(field_path(p), Value::Int(1));
        let union = |p: &str| FieldOp::ArrayUnion(field_path(p), vec![Value::Int(1)]);
        let remove = |p: &str| FieldOp::ArrayRemove(field_path(p), vec![Value::Int(1)]);
        let conflicts = [
            vec![set("a"), set("a.b")],
            vec![set("a.b"), set("a")],
            vec![set("a"), set("a")],
            vec![set("tags"), union("tags")],
            vec![union("tags"), remove("tags")],
            vec![union("a.b"), set("a")],
        ];
        for ops in conflicts {
            let r = write(&update(ops.clone(), Guard::None), &named());
            assert!(
                matches!(&r, Err(GatewayError::Other(m)) if m.contains("conflicting")),
                "{ops:?}: {r:?}"
            );
        }
    }

    #[test]
    fn sibling_and_similarly_named_update_paths_do_not_conflict() {
        let set = |p: &str| FieldOp::Set(field_path(p), Value::Int(1));
        let ops = vec![set("a.b"), set("a.c"), set("ab"), set("`a.b`")];
        assert!(write(&update(ops, Guard::None), &named()).is_ok());
    }
}
