// Generators produce only legal domain values: every id, path and timestamp goes through the
// type's own constructor or parser, so a failing property is a bug in the code, not the generator.
use std::ops::RangeInclusive;

use proptest::collection::vec;
use proptest::option;
use proptest::prelude::*;
use proptest::sample::select;

use super::{
    CollectionId, CollectionPath, DbTarget, DocId, DocPath, DocResourceName, Document, FieldPath,
    Fields, GeoPoint, Timestamp, UpdateTime, Value,
};

// No persistence: the nix build drops `proptest-regressions/`, so a replay file would only hide
// a failure on CI. A real counterexample becomes a named example test instead.
pub(crate) fn config() -> ProptestConfig {
    ProptestConfig {
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

// `any::<String>()` never yields control characters or newlines; `any::<char>()` does.
fn chars(len: RangeInclusive<usize>) -> impl Strategy<Value = String> {
    vec(any::<char>(), len).prop_map(String::from_iter)
}

// Uniform text almost never forms a backtick pair, an escape or a `ts:` prefix, so half of
// the text is built from the syntax alphabet the parsers care about.
pub(crate) fn text() -> impl Strategy<Value = String> {
    let atoms = select(vec![
        "`", "\\", "\"", "=", "==", "+=", "-=", "!=", "<", ">=", ":", "[", "]", ",", "{", "}", ".",
        " ", "-", "0", "1", "a", "b", "in", "not-in", "ts:", "ref:", "true", "null", "1e3",
        "\u{e9}",
    ]);
    prop_oneof![
        1 => chars(0..=8),
        1 => vec(atoms, 0..=12).prop_map(|parts| parts.concat()),
    ]
}

fn field_segment() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => "[a-zA-Z_][a-zA-Z0-9_]{0,6}",
        3 => "[`\\\\. a-z=+:-]{1,4}",
        2 => chars(1..=6),
    ]
    .prop_filter("a segment is not empty", |s| !s.is_empty())
}

pub(crate) fn field_path() -> impl Strategy<Value = FieldPath> {
    vec(field_segment(), 1..=4).prop_map(|segments| FieldPath::from_segments(segments).unwrap())
}

fn id_text() -> impl Strategy<Value = String> {
    let near_reserved = select(vec![
        "projects",
        "databases",
        "documents",
        "__a",
        "a__",
        "___",
        "...",
        ".a",
        "a.b",
        " ",
        "_",
        "__",
    ])
    .prop_map(str::to_owned);
    prop_oneof![
        4 => "[a-zA-Z0-9_-]{1,8}",
        2 => near_reserved,
        2 => chars(1..=6),
    ]
}

fn collection_id() -> impl Strategy<Value = CollectionId> {
    id_text().prop_filter_map("a legal collection id", |s| s.parse().ok())
}

fn doc_id() -> impl Strategy<Value = DocId> {
    id_text().prop_filter_map("a legal document id", |s| s.parse().ok())
}

fn doc_chain(deeper: RangeInclusive<usize>) -> impl Strategy<Value = DocPath> {
    (
        collection_id(),
        doc_id(),
        vec((collection_id(), doc_id()), deeper),
    )
        .prop_map(|(collection, id, rest)| {
            let first = DocPath::new(CollectionPath::new(None, collection), id);
            rest.into_iter().fold(first, |parent, (collection, id)| {
                DocPath::new(CollectionPath::new(Some(parent), collection), id)
            })
        })
}

pub(crate) fn doc_path() -> impl Strategy<Value = DocPath> {
    doc_chain(0..=2)
}

pub(crate) fn collection_path() -> impl Strategy<Value = CollectionPath> {
    (option::of(doc_chain(0..=1)), collection_id())
        .prop_map(|(parent, id)| CollectionPath::new(parent, id))
}

// Firestore's range: 0001-01-01T00:00:00Z to 9999-12-31T23:59:59Z. Later years print as
// `+10000-...` and do not parse back.
const MIN_SECONDS: i64 = -62_135_596_800;
const MAX_SECONDS: i64 = 253_402_300_799;

pub(crate) fn timestamp() -> impl Strategy<Value = Timestamp> {
    let seconds = prop_oneof![
        6 => MIN_SECONDS..=MAX_SECONDS,
        2 => -86_400..=86_400_i64,
        1 => Just(MIN_SECONDS),
        1 => Just(MAX_SECONDS),
    ];
    // The four `AutoSi` widths: no fraction, whole millis, whole micros, full nanos.
    let nanos = prop_oneof![
        1 => Just(0_u32),
        1 => (0..1_000_u32).prop_map(|millis| millis * 1_000_000),
        1 => (0..1_000_000_u32).prop_map(|micros| micros * 1_000),
        2 => 0..1_000_000_000_u32,
    ];
    (seconds, nanos).prop_map(|(s, n)| Timestamp::from_unix(s, n).unwrap())
}

pub(crate) fn update_time() -> impl Strategy<Value = UpdateTime> {
    timestamp().prop_map(UpdateTime::new)
}

// Typed JSON writes every NaN as `"NaN"`, so a NaN's sign and payload do not survive it; every
// NaN agrees with every NaN. Any other double, and a geo point's coordinates, agree by bits.
pub(crate) fn same(a: &Value, b: &Value) -> bool {
    agree(a, b, nan_or_bits)
}

pub(crate) fn same_fields(a: &Fields, b: &Fields) -> bool {
    agree_fields(a, b, nan_or_bits)
}

// Protobuf keeps a double as it is, so there a NaN's payload must survive too.
pub(crate) fn same_bits(a: &Value, b: &Value) -> bool {
    agree(a, b, bits)
}

pub(crate) fn same_fields_bits(a: &Fields, b: &Fields) -> bool {
    agree_fields(a, b, bits)
}

fn nan_or_bits(x: f64, y: f64) -> bool {
    (x.is_nan() && y.is_nan()) || x.to_bits() == y.to_bits()
}

fn bits(x: f64, y: f64) -> bool {
    x.to_bits() == y.to_bits()
}

fn agree(a: &Value, b: &Value, double: fn(f64, f64) -> bool) -> bool {
    match (a, b) {
        (Value::Double(x), Value::Double(y)) => double(*x, *y),
        (Value::Geo(x), Value::Geo(y)) => double(x.lat(), y.lat()) && double(x.lng(), y.lng()),
        (Value::Array(xs), Value::Array(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| agree(x, y, double))
        }
        (Value::Map(xs), Value::Map(ys)) => agree_fields(xs, ys, double),
        _ => a == b,
    }
}

fn agree_fields(a: &Fields, b: &Fields, double: fn(f64, f64) -> bool) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|((ka, x), (kb, y))| ka == kb && agree(x, y, double))
}

// The target the protobuf properties fix; `foreign_ref` never lands in it, because a name inside
// the target decodes as a `Ref`.
const FIXED_PROJECT: &str = "p";
const FIXED_DATABASE: &str = "d";

pub(crate) fn fixed_target() -> DbTarget {
    target(FIXED_PROJECT, FIXED_DATABASE)
}

fn target(project: &str, database: &str) -> DbTarget {
    DbTarget {
        project: project.parse().unwrap(),
        database: database.parse().unwrap(),
    }
}

pub(crate) fn db_target() -> impl Strategy<Value = DbTarget> {
    let project = prop_oneof![
        1 => Just(FIXED_PROJECT.to_owned()),
        1 => Just("q".to_owned()),
        2 => "[a-z0-9-]{1,5}",
    ];
    let database = prop_oneof![
        1 => Just(FIXED_DATABASE.to_owned()),
        1 => Just("d2".to_owned()),
        1 => Just("d-2".to_owned()),
        1 => Just("(default)".to_owned()),
        2 => "[a-z0-9-]{1,5}",
    ];
    (project, database).prop_map(|(project, database)| target(&project, &database))
}

fn foreign_ref() -> impl Strategy<Value = DocResourceName> {
    let outside = db_target().prop_filter("outside the fixed target", |t| *t != fixed_target());
    (outside, doc_path())
        .prop_map(|(t, path)| format!("{}/{path}", t.documents_root()).parse().unwrap())
}

// Uniform `i64` never reaches the extremes; 2^53 + 1 is the first integer an `f64` cannot hold.
fn int() -> impl Strategy<Value = i64> {
    prop_oneof![
        4 => any::<i64>(),
        2 => -1_000..=1_000_i64,
        1 => Just(i64::MIN),
        1 => Just(i64::MAX),
        1 => Just(9_007_199_254_740_993_i64),
    ]
}

// `f64::ANY` yields no NaN in practice, so the classes the codecs special-case are named.
fn double() -> impl Strategy<Value = f64> {
    prop_oneof![
        4 => prop::num::f64::ANY,
        2 => (-1_000..=1_000_i32).prop_map(f64::from),
        1 => Just(f64::NAN),
        1 => Just(0.0),
        1 => Just(-0.0),
        1 => Just(f64::INFINITY),
        1 => Just(f64::NEG_INFINITY),
        1 => Just(f64::MIN_POSITIVE),
        1 => Just(5e-324),
        1 => Just(f64::MAX),
        1 => Just(9.3e18),
    ]
}

fn coordinate(bound: f64) -> impl Strategy<Value = f64> {
    prop_oneof![
        4 => -bound..=bound,
        1 => Just(bound),
        1 => Just(-bound),
        1 => Just(0.0),
        1 => Just(-0.0),
        1 => Just(5e-324),
    ]
}

fn geo_point() -> impl Strategy<Value = GeoPoint> {
    (coordinate(90.0), coordinate(180.0)).prop_map(|(lat, lng)| GeoPoint::new(lat, lng).unwrap())
}

// A sole `$` key is the shape the `$map` escape exists for; `_path` and `data` collide with the
// document envelope; the slash and tilde are what a JSON pointer escapes.
fn key() -> BoxedStrategy<String> {
    let special = select(vec![
        "$ts",
        "$map",
        "$double",
        "$geo",
        "$ref",
        "$bytes",
        "_path",
        "_updateTime",
        "_plain",
        "_select",
        "data",
        "a/b~c",
    ])
    .prop_map(str::to_owned);
    prop_oneof![
        3 => text(),
        2 => "\\$[a-z]{0,3}",
        3 => special,
    ]
    .boxed()
}

fn leaf() -> BoxedStrategy<Value> {
    prop_oneof![
        1 => Just(Value::Null),
        1 => any::<bool>().prop_map(Value::Bool),
        2 => int().prop_map(Value::Int),
        3 => double().prop_map(Value::Double),
        3 => text().prop_map(Value::String),
        2 => timestamp().prop_map(Value::Timestamp),
        1 => vec(any::<u8>(), 0..=8).prop_map(Value::Bytes),
        2 => doc_path().prop_map(Value::Ref),
        2 => foreign_ref().prop_map(Value::ForeignRef),
        2 => geo_point().prop_map(Value::Geo),
    ]
    .boxed()
}

// Weighted to 0 and 1 entries: only a map with exactly one entry can be the `$map` escape.
fn entries(inner: BoxedStrategy<Value>) -> BoxedStrategy<Fields> {
    let entry = (key(), inner);
    prop_oneof![
        3 => Just(Fields::new()),
        3 => entry.clone().prop_map(|(k, v)| Fields::from([(k, v)])),
        2 => vec(entry, 2..=6).prop_map(Fields::from_iter),
    ]
    .boxed()
}

// Firestore forbids an array directly inside an array, so an array holds only `non_array` values.
fn value_at(depth: u32) -> BoxedStrategy<Value> {
    match depth.checked_sub(1) {
        None => leaf(),
        Some(below) => prop_oneof![
            3 => leaf(),
            2 => entries(value_at(below)).prop_map(Value::Map),
            2 => vec(non_array(below), 0..=6).prop_map(Value::Array),
        ]
        .boxed(),
    }
}

fn non_array(depth: u32) -> BoxedStrategy<Value> {
    match depth.checked_sub(1) {
        None => leaf(),
        Some(below) => prop_oneof![
            3 => leaf(),
            2 => entries(value_at(below)).prop_map(Value::Map),
        ]
        .boxed(),
    }
}

pub(crate) fn value() -> BoxedStrategy<Value> {
    value_at(3)
}

// One level shallower than `value`: a document's `data` is itself a map.
pub(crate) fn fields() -> BoxedStrategy<Fields> {
    entries(value_at(2))
}

pub(crate) fn document() -> impl Strategy<Value = Document> {
    (doc_path(), update_time(), fields()).prop_map(|(path, update_time, data)| Document {
        path,
        update_time,
        data,
    })
}

#[cfg(test)]
mod tests {
    use proptest::strategy::ValueTree as _;
    use proptest::test_runner::TestRunner;

    use super::*;

    fn sample<S: Strategy>(strategy: &S) -> Vec<S::Value> {
        let mut runner = TestRunner::deterministic();
        (0..1_000)
            .map(|_| strategy.new_tree(&mut runner).unwrap().current())
            .collect()
    }

    fn reaches<T>(items: &[T], class: &str, is_member: impl Fn(&T) -> bool) {
        assert!(items.iter().any(is_member), "no sample in class: {class}");
    }

    fn is_identifier(s: &str) -> bool {
        s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    }

    #[test]
    fn text_reaches_its_classes() {
        let texts = sample(&text());
        reaches(&texts, "empty", String::is_empty);
        reaches(&texts, "backtick", |s| s.contains('`'));
        reaches(&texts, "double quote", |s| s.contains('"'));
        reaches(&texts, "backslash", |s| s.contains('\\'));
        reaches(&texts, "open brace", |s| s.contains('{'));
        reaches(&texts, "list brackets", |s| {
            s.contains('[') && s.contains(']')
        });
        reaches(&texts, "timestamp prefix", |s| s.contains("ts:"));
        reaches(&texts, "newline", |s| s.contains('\n'));
        reaches(&texts, "control character", |s| {
            s.chars().any(char::is_control)
        });
        reaches(&texts, "non-ASCII", |s| !s.is_ascii());
    }

    #[test]
    fn field_paths_reach_their_classes() {
        let paths = sample(&field_path());
        let segments: Vec<&String> = paths.iter().flat_map(FieldPath::segments).collect();
        reaches(&paths, "one segment", |p| p.segments().len() == 1);
        reaches(&paths, "four segments", |p| p.segments().len() == 4);
        reaches(&segments, "identifier", |s| is_identifier(s));
        reaches(&segments, "backtick", |s| s.contains('`'));
        reaches(&segments, "backslash", |s| s.contains('\\'));
        reaches(&segments, "dot", |s| s.contains('.'));
        reaches(&segments, "space", |s| s.contains(' '));
        reaches(&segments, "equals", |s| s.contains('='));
        reaches(&segments, "colon", |s| s.contains(':'));
        reaches(&segments, "trailing plus", |s| s.ends_with('+'));
        reaches(&segments, "trailing minus", |s| s.ends_with('-'));
        reaches(&segments, "leading digit", |s| {
            s.starts_with(|c: char| c.is_ascii_digit())
        });
        reaches(&segments, "control character", |s| {
            s.chars().any(char::is_control)
        });
        reaches(&segments, "non-ASCII", |s| !s.is_ascii());
    }

    #[test]
    fn doc_and_collection_paths_reach_their_classes() {
        let docs = sample(&doc_path());
        reaches(&docs, "one pair", |d| d.collection().parent().is_none());
        reaches(&docs, "three pairs", |d| {
            d.collection()
                .parent()
                .and_then(|p| p.collection().parent())
                .is_some()
        });
        reaches(&docs, "collection named projects", |d| {
            d.collection().id().as_str() == "projects"
        });
        reaches(&docs, "id with a space", |d| d.id().as_str().contains(' '));
        reaches(&docs, "id like __a", |d| d.id().as_str() == "__a");
        reaches(&docs, "id of dots", |d| d.id().as_str() == "...");
        let collections = sample(&collection_path());
        reaches(&collections, "top level", |c| c.parent().is_none());
        reaches(&collections, "nested", |c| c.parent().is_some());
        reaches(&collections, "two pairs of parents", |c| {
            c.parent().and_then(|p| p.collection().parent()).is_some()
        });
    }

    #[test]
    fn timestamps_reach_their_classes() {
        let times = sample(&timestamp());
        reaches(&times, "no fraction", |t| t.nanos() == 0);
        reaches(&times, "whole millis", |t| {
            t.nanos() != 0 && t.nanos() % 1_000_000 == 0
        });
        reaches(&times, "whole micros", |t| {
            t.nanos() % 1_000 == 0 && t.nanos() % 1_000_000 != 0
        });
        reaches(&times, "full nanos", |t| t.nanos() % 1_000 != 0);
        reaches(&times, "earliest second", |t| t.seconds() == MIN_SECONDS);
        reaches(&times, "latest second", |t| t.seconds() == MAX_SECONDS);
        reaches(&times, "before 1970", |t| t.seconds() < 0);
        reaches(&times, "999999999 nanos is reachable", |t| {
            t.nanos() > 999_000_000
        });
        let updates = sample(&update_time());
        reaches(&updates, "full nanos", |u| {
            u.timestamp().nanos() % 1_000 != 0
        });
    }
    fn walk<'a>(v: &'a Value, depth: usize, out: &mut Vec<(usize, &'a Value)>) {
        out.push((depth, v));
        match v {
            Value::Array(xs) => {
                for x in xs {
                    walk(x, depth + 1, out);
                }
            }
            Value::Map(m) => {
                for x in m.values() {
                    walk(x, depth + 1, out);
                }
            }
            Value::Null
            | Value::Bool(_)
            | Value::Int(_)
            | Value::Double(_)
            | Value::String(_)
            | Value::Timestamp(_)
            | Value::Bytes(_)
            | Value::Ref(_)
            | Value::ForeignRef(_)
            | Value::Geo(_) => {}
        }
    }

    fn nodes_of(trees: &[Value]) -> Vec<(usize, &Value)> {
        let mut nodes = Vec::new();
        for tree in trees {
            walk(tree, 0, &mut nodes);
        }
        nodes
    }

    fn map_depth(v: &Value) -> usize {
        match v {
            Value::Map(m) => 1 + m.values().map(map_depth).max().unwrap_or(0),
            Value::Array(xs) => xs.iter().map(map_depth).max().unwrap_or(0),
            Value::Null
            | Value::Bool(_)
            | Value::Int(_)
            | Value::Double(_)
            | Value::String(_)
            | Value::Timestamp(_)
            | Value::Bytes(_)
            | Value::Ref(_)
            | Value::ForeignRef(_)
            | Value::Geo(_) => 0,
        }
    }

    fn is_sole_dollar_map(v: &Value) -> bool {
        matches!(v, Value::Map(m) if m.len() == 1 && m.keys().all(|k| k.starts_with('$')))
    }

    fn is(x: f64, y: f64) -> bool {
        x.to_bits() == y.to_bits()
    }

    #[test]
    fn doubles_and_ints_reach_their_classes() {
        let trees = sample(&value());
        let nodes = nodes_of(&trees);
        let doubles: Vec<f64> = nodes
            .iter()
            .filter_map(|(_, v)| {
                if let Value::Double(d) = v {
                    Some(*d)
                } else {
                    None
                }
            })
            .collect();
        reaches(&doubles, "NaN", |d| d.is_nan());
        reaches(&doubles, "negative NaN", |d| {
            d.is_nan() && d.is_sign_negative()
        });
        reaches(&doubles, "+inf", |d| *d == f64::INFINITY);
        reaches(&doubles, "-inf", |d| *d == f64::NEG_INFINITY);
        reaches(&doubles, "-0.0", |d| is(*d, -0.0));
        reaches(&doubles, "+0.0", |d| is(*d, 0.0));
        reaches(&doubles, "subnormal", |d| d.is_subnormal());
        reaches(&doubles, "integral beyond i64", |d| {
            d.is_finite() && d.fract() == 0.0 && d.abs() >= 9.2e18
        });
        let ints: Vec<i64> = nodes
            .iter()
            .filter_map(|(_, v)| {
                if let Value::Int(i) = v {
                    Some(*i)
                } else {
                    None
                }
            })
            .collect();
        reaches(&ints, "i64::MIN", |i| *i == i64::MIN);
        reaches(&ints, "i64::MAX", |i| *i == i64::MAX);
        reaches(&ints, "negative", |i| *i < 0);
    }

    #[test]
    fn leaves_reach_their_classes() {
        let trees = sample(&value());
        let all: Vec<&Value> = nodes_of(&trees).into_iter().map(|(_, v)| v).collect();
        reaches(&all, "null", |v| matches!(v, Value::Null));
        reaches(&all, "bool", |v| matches!(v, Value::Bool(_)));
        reaches(&all, "string", |v| matches!(v, Value::String(_)));
        reaches(
            &all,
            "empty string",
            |v| matches!(v, Value::String(s) if s.is_empty()),
        );
        reaches(
            &all,
            "bytes",
            |v| matches!(v, Value::Bytes(b) if !b.is_empty()),
        );
        reaches(
            &all,
            "nanosecond timestamp",
            |v| matches!(v, Value::Timestamp(t) if t.nanos() % 1_000 != 0),
        );
        reaches(
            &all,
            "ref under a collection named projects",
            |v| matches!(v, Value::Ref(p) if p.to_string().starts_with("projects/")),
        );
        reaches(
            &all,
            "geo at +90 latitude",
            |v| matches!(v, Value::Geo(g) if is(g.lat(), 90.0)),
        );
        reaches(
            &all,
            "geo at -90 latitude",
            |v| matches!(v, Value::Geo(g) if is(g.lat(), -90.0)),
        );
        reaches(
            &all,
            "geo at +180 longitude",
            |v| matches!(v, Value::Geo(g) if is(g.lng(), 180.0)),
        );
        reaches(
            &all,
            "geo at -180 longitude",
            |v| matches!(v, Value::Geo(g) if is(g.lng(), -180.0)),
        );
        reaches(
            &all,
            "geo with -0.0",
            |v| matches!(v, Value::Geo(g) if is(g.lat(), -0.0)),
        );
    }

    #[test]
    fn containers_reach_their_classes() {
        let trees = sample(&value());
        let nodes = nodes_of(&trees);
        let all: Vec<&Value> = nodes.iter().map(|&(_, v)| v).collect();
        reaches(
            &all,
            "empty array",
            |v| matches!(v, Value::Array(xs) if xs.is_empty()),
        );
        reaches(
            &all,
            "empty map",
            |v| matches!(v, Value::Map(m) if m.is_empty()),
        );
        reaches(
            &all,
            "array of maps",
            |v| matches!(v, Value::Array(xs) if xs.iter().any(|x| matches!(x, Value::Map(_)))),
        );
        reaches(
            &all,
            "six-element array",
            |v| matches!(v, Value::Array(xs) if xs.len() == 6),
        );
        reaches(
            &all,
            "map with several entries",
            |v| matches!(v, Value::Map(m) if m.len() >= 3),
        );
        reaches(&all, "map nested three deep", |v| map_depth(v) == 3);
        reaches(&nodes, "nested sole-$ map", |&(depth, v)| {
            depth > 0 && is_sole_dollar_map(v)
        });
        reaches(
            &all,
            "sole-$ map in an array",
            |v| matches!(v, Value::Array(xs) if xs.iter().any(is_sole_dollar_map)),
        );
        reaches(
            &all,
            "sole `$map` key",
            |v| matches!(v, Value::Map(m) if m.len() == 1 && m.contains_key("$map")),
        );
        reaches(
            &all,
            "sole `$double` key",
            |v| matches!(v, Value::Map(m) if m.len() == 1 && m.contains_key("$double")),
        );
        reaches(
            &all,
            "key `_path`",
            |v| matches!(v, Value::Map(m) if m.contains_key("_path")),
        );
        reaches(
            &all,
            "key `data`",
            |v| matches!(v, Value::Map(m) if m.contains_key("data")),
        );
        reaches(
            &all,
            "empty key",
            |v| matches!(v, Value::Map(m) if m.contains_key("")),
        );
    }

    #[test]
    fn values_are_always_legal() {
        let trees = sample(&value());
        let fixed = format!("{}/", fixed_target().documents_root());
        for (_, v) in nodes_of(&trees) {
            match v {
                Value::Array(xs) => {
                    assert!(xs.len() <= 6);
                    assert!(!xs.iter().any(|x| matches!(x, Value::Array(_))), "{v:?}");
                }
                Value::Map(m) => assert!(m.len() <= 6),
                Value::ForeignRef(name) => {
                    assert!(!name.as_str().starts_with(&fixed), "{name}");
                }
                Value::Null
                | Value::Bool(_)
                | Value::Int(_)
                | Value::Double(_)
                | Value::String(_)
                | Value::Timestamp(_)
                | Value::Bytes(_)
                | Value::Ref(_)
                | Value::Geo(_) => {}
            }
        }
        assert!(trees.iter().all(|t| map_depth(t) <= 3));
    }

    #[test]
    fn foreign_refs_and_targets_reach_their_classes() {
        let trees = sample(&value());
        let names: Vec<&str> = nodes_of(&trees)
            .iter()
            .filter_map(|(_, v)| {
                if let Value::ForeignRef(n) = v {
                    Some(n.as_str())
                } else {
                    None
                }
            })
            .collect();
        reaches(&names, "same project, other database", |n| {
            n.starts_with("projects/p/databases/") && !n.starts_with("projects/p/databases/d/")
        });
        reaches(&names, "sibling database d2", |n| {
            n.starts_with("projects/p/databases/d2/")
        });
        reaches(&names, "other project", |n| {
            !n.starts_with("projects/p/databases/")
        });
        reaches(&names, "default database", |n| {
            n.contains("/databases/(default)/")
        });

        let targets = sample(&db_target());
        reaches(&targets, "the fixed target", |t| *t == fixed_target());
        reaches(&targets, "default database", |t| {
            t.database.as_str() == "(default)"
        });
        reaches(&targets, "other project", |t| t.project.as_str() != "p");
    }

    #[test]
    fn documents_reach_their_classes() {
        let docs = sample(&document());
        reaches(&docs, "empty data", |d| d.data.is_empty());
        reaches(&docs, "data with several fields", |d| d.data.len() >= 3);
        reaches(&docs, "a field named _path", |d| {
            d.data.contains_key("_path")
        });
        reaches(&docs, "a field named data", |d| d.data.contains_key("data"));
        reaches(&docs, "a sole `$` field", |d| {
            d.data.len() == 1 && d.data.keys().all(|k| k.starts_with('$'))
        });
        reaches(&docs, "a nanosecond update time", |d| {
            d.update_time.timestamp().nanos() % 1_000 != 0
        });
        assert!(
            docs.iter()
                .all(|d| d.data.values().all(|v| map_depth(v) <= 2))
        );
    }

    #[test]
    fn same_is_lenient_only_about_nan() {
        let nan = Value::Double(f64::NAN);
        let other_nan = Value::Double(-f64::NAN);
        assert!(same(&nan, &other_nan));
        assert!(!same_bits(&nan, &other_nan));
        let (zero, negative) = (Value::Double(0.0), Value::Double(-0.0));
        assert!(!same(&zero, &negative));
        let geo = |lat| Value::Geo(GeoPoint::new(lat, 0.0).unwrap());
        assert!(!same(&geo(0.0), &geo(-0.0)));
        let nested = |d| Value::Array(vec![Value::Map(Fields::from([("k".to_owned(), d)]))]);
        assert!(same(&nested(nan.clone()), &nested(other_nan)));
        assert!(!same(&nested(zero), &nested(negative)));
        assert!(!same(&nested(nan), &Value::Array(vec![])));
        let a = Fields::from([("a".to_owned(), Value::Null)]);
        let b = Fields::from([("b".to_owned(), Value::Null)]);
        assert!(same_fields(&a, &a) && same_fields_bits(&a, &a));
        assert!(!same_fields(&a, &b) && !same_fields(&a, &Fields::new()));
    }
}
