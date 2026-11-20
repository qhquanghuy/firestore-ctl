// Generators produce only legal domain values: every id, path and timestamp goes through the
// type's own constructor or parser, so a failing property is a bug in the code, not the generator.
use std::ops::RangeInclusive;

use proptest::collection::vec;
use proptest::option;
use proptest::prelude::*;
use proptest::sample::select;

use super::{CollectionId, CollectionPath, DocId, DocPath, FieldPath, Timestamp, UpdateTime};

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
}
