//! Index advice: which index a query needs and how to create it.

use itertools::Itertools;

use super::field::FieldPath;
use super::ids::{CollectionId, DbTarget};
use super::nonempty::NonEmptyVec;
use super::query::{Direction, Filter, Op, Query, Scope};

/// How one field participates in a composite index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexMode {
    /// Ascending order.
    Asc,
    /// Descending order.
    Desc,
    /// Array-contains matching.
    ArrayContains,
}

/// One field of an index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexField {
    /// The indexed field.
    pub field: FieldPath,
    /// How it is indexed.
    pub mode: IndexMode,
}

/// Whether an index serves one collection or every collection of the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryScope {
    /// A single collection.
    Collection,
    /// All collections with the group's name.
    CollectionGroup,
}

/// A composite index definition. [`required_index`] only produces specs with at least
/// two fields, the minimum Firestore accepts for a composite index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexSpec {
    /// The collection (or group) name the index is on.
    pub collection_group: CollectionId,
    /// Single collection or collection group.
    pub scope: QueryScope,
    /// Indexed fields, in index order.
    pub fields: NonEmptyVec<IndexField>,
}

/// The index a query is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexAdvice {
    /// A composite index over two or more fields.
    Composite(IndexSpec),
    /// A collection-group-scoped single-field index (not created by default, and
    /// Firestore rejects one-field composite indexes).
    SingleField {
        /// The collection group name.
        collection_group: CollectionId,
        /// The field and how it must be indexed.
        field: IndexField,
    },
}

/// What index `q` needs beyond Firestore's automatic indexes, if any.
///
/// Candidate fields, in order, each at its first position only:
/// 1. equality filters (`==`, `in`: ascending; `array-contains[-any]`: array-contains)
///    in filter order;
/// 2. each `order` term with its direction;
/// 3. fields of inequality filters (`!=`, `not-in`, `<`, `<=`, `>`, `>=`) not yet
///    present, sorted by field path, in the direction of the last `order` term
///    (ascending if there is none), as Firestore orders them implicitly.
///
/// The document name (`__name__`) is never a candidate. For a collection scope the
/// result is [`IndexAdvice::Composite`] when there are at least two candidates and at
/// least one ordering or inequality; equality-only queries are served by merging
/// single-field indexes. A collection-group scope has no automatic indexes: two or more
/// candidates give a composite, exactly one gives [`IndexAdvice::SingleField`].
#[must_use]
pub fn required_index(q: &Query) -> Option<IndexAdvice> {
    let is_equality = |f: &&Filter| {
        matches!(
            f.op(),
            Op::Eq | Op::In | Op::ArrayContains | Op::ArrayContainsAny
        )
    };
    let indexable = |field: &FieldPath| field.segments() != ["__name__"];
    let (equalities, inequalities): (Vec<&Filter>, Vec<&Filter>) =
        q.filters.iter().partition(is_equality);
    let orders: Vec<IndexField> = q
        .order
        .iter()
        .filter(|o| indexable(&o.field))
        .map(|o| IndexField {
            field: o.field.clone(),
            mode: direction_mode(o.direction),
        })
        .collect();
    let implicit_mode = q
        .order
        .last()
        .map_or(IndexMode::Asc, |o| direction_mode(o.direction));
    let implicit: Vec<IndexField> = inequalities
        .iter()
        .map(|f| f.field())
        .filter(|field| indexable(field))
        .sorted()
        .dedup()
        .map(|field| IndexField {
            field: field.clone(),
            mode: implicit_mode,
        })
        .collect();
    let beyond_equality = !orders.is_empty() || !implicit.is_empty();

    let fields: Vec<IndexField> = equalities
        .into_iter()
        .filter(|f| indexable(f.field()))
        .map(|f| IndexField {
            field: f.field().clone(),
            mode: match f.op() {
                Op::ArrayContains | Op::ArrayContainsAny => IndexMode::ArrayContains,
                _ => IndexMode::Asc,
            },
        })
        .chain(orders)
        .chain(implicit)
        .unique_by(|f| f.field.clone())
        .collect();

    let (scope, collection_group) = match &q.scope {
        Scope::Collection(c) => (QueryScope::Collection, c.id().clone()),
        Scope::Group { id, .. } => (QueryScope::CollectionGroup, id.clone()),
    };
    match (scope, fields.as_slice()) {
        (_, []) => None,
        (QueryScope::CollectionGroup, [only]) => Some(IndexAdvice::SingleField {
            collection_group,
            field: only.clone(),
        }),
        (QueryScope::Collection, _) if fields.len() < 2 || !beyond_equality => None,
        _ => NonEmptyVec::try_from(fields).ok().map(|fields| {
            IndexAdvice::Composite(IndexSpec {
                collection_group,
                scope,
                fields,
            })
        }),
    }
}

fn direction_mode(direction: Direction) -> IndexMode {
    match direction {
        Direction::Asc => IndexMode::Asc,
        Direction::Desc => IndexMode::Desc,
    }
}

impl IndexAdvice {
    /// A shell command creating the advised index in `t`'s database.
    #[must_use]
    pub fn gcloud_command(&self, t: &DbTarget) -> String {
        match self {
            Self::Composite(spec) => spec.gcloud_command(t),
            Self::SingleField {
                collection_group,
                field,
            } => {
                let config = match field.mode {
                    IndexMode::Asc => "order=ascending",
                    IndexMode::Desc => "order=descending",
                    IndexMode::ArrayContains => "array-config=contains",
                };
                shell_join(&[
                    "gcloud firestore indexes fields update".to_owned(),
                    field.field.to_firestore(),
                    format!("--project={}", t.project),
                    format!("--database={}", t.database),
                    format!("--collection-group={collection_group}"),
                    format!("--index={config},query-scope=collection-group"),
                ])
            }
        }
    }
}

impl IndexSpec {
    /// A shell command creating this composite index in `t`'s database.
    #[must_use]
    pub fn gcloud_command(&self, t: &DbTarget) -> String {
        let scope = match self.scope {
            QueryScope::Collection => "COLLECTION",
            QueryScope::CollectionGroup => "COLLECTION_GROUP",
        };
        let fields = self.fields.iter().map(|f| {
            let config = match f.mode {
                IndexMode::Asc => "order=ascending",
                IndexMode::Desc => "order=descending",
                IndexMode::ArrayContains => "array-config=contains",
            };
            format!(
                "--field-config=field-path={},{config}",
                f.field.to_firestore()
            )
        });
        let words: Vec<String> = [
            "gcloud firestore indexes composite create".to_owned(),
            format!("--project={}", t.project),
            format!("--database={}", t.database),
            format!("--collection-group={}", self.collection_group),
            format!("--query-scope={scope}"),
        ]
        .into_iter()
        .chain(fields)
        .collect();
        shell_join(&words)
    }
}

/// Joins a fixed command prefix (the first word) and quoted arguments with spaces.
fn shell_join(words: &[String]) -> String {
    words
        .iter()
        .enumerate()
        .map(|(i, w)| if i == 0 { w.clone() } else { shell_quote(w) })
        .join(" ")
}

/// `arg` single-quoted for a POSIX shell unless it is already plain.
fn shell_quote(arg: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "-_=,.:/@+".contains(c);
    if arg.chars().all(plain) {
        arg.to_owned()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::domain::{DbTarget, Filter, OrderBy, Query, Scope};

    fn query(coll: &str, filters: &[&str], orders: &[&str]) -> Query {
        Query {
            scope: Scope::Collection(coll.parse().unwrap()),
            filters: filters
                .iter()
                .map(|f| f.parse::<Filter>().unwrap())
                .collect(),
            order: orders
                .iter()
                .map(|o| o.parse::<OrderBy>().unwrap())
                .collect(),
            limit: None,
            offset: 0,
            select: None,
        }
    }

    fn group(coll: &str, filters: &[&str], orders: &[&str]) -> Query {
        Query {
            scope: Scope::Group {
                parent: None,
                id: coll.parse().unwrap(),
            },
            ..query(coll, filters, orders)
        }
    }

    fn target(project: &str, database: &str) -> DbTarget {
        DbTarget {
            project: project.parse().unwrap(),
            database: database.parse().unwrap(),
        }
    }

    /// The composite index `q` needs; fails the test for any other advice.
    fn composite(q: &Query) -> IndexSpec {
        match required_index(q) {
            Some(IndexAdvice::Composite(spec)) => spec,
            other => panic!("expected a composite index, got {other:?}"),
        }
    }

    fn fields(spec: &IndexSpec) -> Vec<(String, IndexMode)> {
        spec.fields
            .iter()
            .map(|f| (f.field.to_firestore(), f.mode))
            .collect()
    }

    fn asc(name: &str) -> (String, IndexMode) {
        (name.to_owned(), IndexMode::Asc)
    }

    fn desc(name: &str) -> (String, IndexMode) {
        (name.to_owned(), IndexMode::Desc)
    }

    #[test]
    fn index_needed_for_eq_plus_order_on_other_field() {
        let q = query("users", &["status == active"], &["createdAt:desc"]);
        let spec = composite(&q);
        assert_eq!(
            spec.fields
                .iter()
                .map(|f| f.field.to_firestore())
                .collect::<Vec<_>>(),
            ["status", "createdAt"]
        );
    }

    #[test]
    fn no_index_for_simple_queries() {
        assert!(required_index(&query("users", &["status == active"], &[])).is_none());
        assert!(
            required_index(&query("users", &["status == active", "plan == pro"], &[])).is_none()
        );
        assert!(required_index(&query("users", &["age > 18"], &["age"])).is_none());
        assert!(required_index(&query("users", &[], &[])).is_none());
    }

    #[test]
    fn gcloud_command_names_database() {
        let cmd = composite(&query("users", &["status == active", "age > 18"], &[]))
            .gcloud_command(&target("p", "app-db"));
        assert!(
            cmd.contains("--database=app-db")
                && cmd.contains("--collection-group=users")
        );
    }

    #[test]
    fn fields_follow_equality_then_order_then_inequality() {
        let q = query(
            "users",
            &["tags array-contains x", "age > 18", "status in [a, b]"],
            &["createdAt:desc"],
        );
        assert_eq!(
            fields(&composite(&q)),
            [
                ("tags".to_owned(), IndexMode::ArrayContains),
                asc("status"),
                desc("createdAt"),
                desc("age"),
            ]
        );
    }

    #[test]
    fn implicit_inequality_fields_are_sorted_by_path_in_last_order_direction() {
        let q = query("users", &["z > 1", "a < 5", "e == 1"], &[]);
        assert_eq!(fields(&composite(&q)), [asc("e"), asc("a"), asc("z")]);
        let q = query("users", &["z > 1", "a < 5"], &["c:desc", "d:asc"]);
        assert_eq!(
            fields(&composite(&q)),
            [desc("c"), asc("d"), asc("a"), asc("z")]
        );
        let q = query("users", &["z > 1", "a < 5"], &["c:asc", "d:desc"]);
        assert_eq!(
            fields(&composite(&q)),
            [asc("c"), desc("d"), desc("a"), desc("z")]
        );
    }

    #[test]
    fn ne_and_not_in_count_as_inequalities_and_array_contains_any_as_array() {
        let ne = query("users", &["a == 1", "b != 2"], &[]);
        assert_eq!(fields(&composite(&ne)), [asc("a"), asc("b")]);
        let not_in = query("users", &["a == 1", "b not-in [1, 2]"], &[]);
        assert_eq!(fields(&composite(&not_in)), [asc("a"), asc("b")]);
        let any = query("users", &["t array-contains-any [x, y]"], &["c"]);
        assert_eq!(
            fields(&composite(&any)),
            [("t".to_owned(), IndexMode::ArrayContains), asc("c")]
        );
    }

    #[test]
    fn ordered_inequality_field_is_not_repeated() {
        let q = query("users", &["status == a", "age > 18"], &["age:desc"]);
        assert_eq!(fields(&composite(&q)), [asc("status"), desc("age")]);
    }

    #[test]
    fn document_name_is_not_an_index_field() {
        assert!(required_index(&query("users", &["a == 1"], &["__name__"])).is_none());
        assert!(required_index(&query("users", &["__name__ > x"], &["a"])).is_none());
        let q = query("users", &["a == 1"], &["b", "__name__:desc"]);
        assert_eq!(fields(&composite(&q)), [asc("a"), asc("b")]);
    }

    #[test]
    fn collection_group_single_field_gets_field_override_advice() {
        let q = group("users", &["status == a"], &[]);
        assert_eq!(
            required_index(&q),
            Some(IndexAdvice::SingleField {
                collection_group: "users".parse().unwrap(),
                field: IndexField {
                    field: "status".parse().unwrap(),
                    mode: IndexMode::Asc,
                },
            })
        );
        assert!(matches!(
            required_index(&group("users", &[], &["age:desc"])),
            Some(IndexAdvice::SingleField { .. })
        ));
        assert!(required_index(&group("users", &[], &[])).is_none());
    }

    #[test]
    fn collection_group_multi_field_is_a_group_scoped_composite() {
        let spec = composite(&group("users", &["status == a", "plan == b"], &[]));
        assert_eq!(spec.scope, QueryScope::CollectionGroup);
        assert!(
            spec.gcloud_command(&target("p", "d"))
                .contains("--query-scope=COLLECTION_GROUP")
        );
    }

    #[test]
    fn composite_command_has_exact_shape() {
        let advice = required_index(&query(
            "users",
            &["tags array-contains x"],
            &["createdAt:desc"],
        ))
        .unwrap();
        assert_eq!(
            advice.gcloud_command(&target("p", "(default)")),
            "gcloud firestore indexes composite create --project=p '--database=(default)' \
             --collection-group=users --query-scope=COLLECTION \
             --field-config=field-path=tags,array-config=contains \
             --field-config=field-path=createdAt,order=descending"
        );
    }

    #[test]
    fn single_field_commands_have_exact_shape() {
        let t = target("p", "d");
        let ordered = required_index(&group("users", &[], &["age:desc"])).unwrap();
        assert_eq!(
            ordered.gcloud_command(&t),
            "gcloud firestore indexes fields update age --project=p --database=d \
             --collection-group=users --index=order=descending,query-scope=collection-group"
        );
        let array = required_index(&group("users", &["tags array-contains x"], &[])).unwrap();
        assert_eq!(
            array.gcloud_command(&t),
            "gcloud firestore indexes fields update tags --project=p --database=d \
             --collection-group=users --index=array-config=contains,query-scope=collection-group"
        );
    }

    #[test]
    fn gcloud_command_shell_quotes_odd_field_paths() {
        let cmd = composite(&query("users", &["`my field` == 1"], &["b"]))
            .gcloud_command(&target("p", "d"));
        assert!(
            cmd.contains("'--field-config=field-path=`my field`,order=ascending'"),
            "{cmd}"
        );
        let single = required_index(&group("users", &[], &["`my field`"])).unwrap();
        assert!(
            single
                .gcloud_command(&target("p", "d"))
                .contains("update '`my field`' --project=p"),
        );
    }
}
