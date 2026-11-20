//! Composite-index advice: which index a query needs and how to create it.

use itertools::Itertools;

use super::field::FieldPath;
use super::ids::{CollectionId, DbTarget};
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

/// One field of a composite index.
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

/// A composite index definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexSpec {
    /// The collection (or group) name the index is on.
    pub collection_group: CollectionId,
    /// Single collection or collection group.
    pub scope: QueryScope,
    /// Indexed fields, in index order.
    pub fields: Vec<IndexField>,
}

/// The composite index `q` needs, or `None` when Firestore's automatic
/// single-field indexes suffice.
///
/// Fields, in order: equality filters (`==`, `in`: ascending; `array-contains[-any]`:
/// array-contains) in filter order, then each `order` term with its direction, then
/// inequality-filter fields (`!=`, `not-in`, `<`, `<=`, `>`, `>=`) not already present
/// (ascending). A field appears once, at its first position.
///
/// For a collection scope the result is `None` when that list has at most one field, or
/// when there are only equality filters and no ordering (Firestore merges single-field
/// indexes for those). For a collection-group scope single-field indexes are not created
/// by default, so any filter or ordering yields `Some`.
#[must_use]
pub fn required_index(q: &Query) -> Option<IndexSpec> {
    let is_equality = |f: &&Filter| {
        matches!(
            f.op(),
            Op::Eq | Op::In | Op::ArrayContains | Op::ArrayContainsAny
        )
    };
    let (equalities, inequalities): (Vec<&Filter>, Vec<&Filter>) =
        q.filters.iter().partition(is_equality);
    let beyond_equality = !q.order.is_empty() || !inequalities.is_empty();

    let fields: Vec<IndexField> = equalities
        .into_iter()
        .map(|f| IndexField {
            field: f.field().clone(),
            mode: match f.op() {
                Op::ArrayContains | Op::ArrayContainsAny => IndexMode::ArrayContains,
                _ => IndexMode::Asc,
            },
        })
        .chain(q.order.iter().map(|o| IndexField {
            field: o.field.clone(),
            mode: match o.direction {
                Direction::Asc => IndexMode::Asc,
                Direction::Desc => IndexMode::Desc,
            },
        }))
        .chain(inequalities.into_iter().map(|f| IndexField {
            field: f.field().clone(),
            mode: IndexMode::Asc,
        }))
        .unique_by(|f| f.field.clone())
        .collect();

    let (scope, collection_group, needed) = match &q.scope {
        Scope::Collection(c) => (
            QueryScope::Collection,
            c.id().clone(),
            fields.len() > 1 && beyond_equality,
        ),
        Scope::Group { id, .. } => (QueryScope::CollectionGroup, id.clone(), !fields.is_empty()),
    };
    needed.then_some(IndexSpec {
        collection_group,
        scope,
        fields,
    })
}

impl IndexSpec {
    /// A shell command creating this index in `t`'s database.
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
        let args = [
            format!("--project={}", t.project),
            format!("--database={}", t.database),
            format!("--collection-group={}", self.collection_group),
            format!("--query-scope={scope}"),
        ]
        .into_iter()
        .chain(fields)
        .map(|arg| shell_quote(&arg))
        .collect::<Vec<_>>();
        format!(
            "gcloud firestore indexes composite create {}",
            args.join(" ")
        )
    }
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
#[allow(clippy::unwrap_used, clippy::expect_used)]
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

    fn fields(spec: &IndexSpec) -> Vec<(String, IndexMode)> {
        spec.fields
            .iter()
            .map(|f| (f.field.to_firestore(), f.mode))
            .collect()
    }

    #[test]
    fn index_needed_for_eq_plus_order_on_other_field() {
        let q = query("users", &["status == active"], &["createdAt:desc"]);
        let spec = required_index(&q).unwrap();
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
        let cmd = required_index(&query("users", &["status == active", "age > 18"], &[]))
            .unwrap()
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
            fields(&required_index(&q).unwrap()),
            [
                ("tags".to_owned(), IndexMode::ArrayContains),
                ("status".to_owned(), IndexMode::Asc),
                ("createdAt".to_owned(), IndexMode::Desc),
                ("age".to_owned(), IndexMode::Asc),
            ]
        );
    }

    #[test]
    fn ne_and_not_in_count_as_inequalities_and_array_contains_any_as_array() {
        let ne = query("users", &["a == 1", "b != 2"], &[]);
        assert_eq!(
            fields(&required_index(&ne).unwrap()),
            [
                ("a".to_owned(), IndexMode::Asc),
                ("b".to_owned(), IndexMode::Asc)
            ]
        );
        let not_in = query("users", &["a == 1", "b not-in [1, 2]"], &[]);
        assert!(required_index(&not_in).is_some());
        let any = query("users", &["t array-contains-any [x, y]"], &["c"]);
        assert_eq!(
            fields(&required_index(&any).unwrap()),
            [
                ("t".to_owned(), IndexMode::ArrayContains),
                ("c".to_owned(), IndexMode::Asc)
            ]
        );
    }

    #[test]
    fn ordered_inequality_field_is_not_repeated() {
        let q = query("users", &["status == a", "age > 18"], &["age:desc"]);
        assert_eq!(
            fields(&required_index(&q).unwrap()),
            [
                ("status".to_owned(), IndexMode::Asc),
                ("age".to_owned(), IndexMode::Desc)
            ]
        );
    }

    #[test]
    fn collection_group_always_needs_index_when_constrained() {
        assert!(required_index(&group("users", &["status == a"], &[])).is_some());
        assert!(required_index(&group("users", &[], &["age"])).is_some());
        assert!(required_index(&group("users", &[], &[])).is_none());
        let spec = required_index(&group("users", &["status == a"], &[])).unwrap();
        assert_eq!(spec.scope, QueryScope::CollectionGroup);
        assert!(
            spec.gcloud_command(&target("p", "d"))
                .contains("--query-scope=COLLECTION_GROUP")
        );
    }

    #[test]
    fn gcloud_command_has_exact_shape() {
        let cmd = required_index(&query(
            "users",
            &["tags array-contains x"],
            &["createdAt:desc"],
        ))
        .unwrap()
        .gcloud_command(&target("p", "(default)"));
        assert_eq!(
            cmd,
            "gcloud firestore indexes composite create --project=p '--database=(default)' \
             --collection-group=users --query-scope=COLLECTION \
             --field-config=field-path=tags,array-config=contains \
             --field-config=field-path=createdAt,order=descending"
        );
    }

    #[test]
    fn gcloud_command_shell_quotes_odd_field_paths() {
        let cmd = required_index(&query("users", &["`my field` == 1"], &["b"]))
            .unwrap()
            .gcloud_command(&target("p", "d"));
        assert!(
            cmd.contains("'--field-config=field-path=`my field`,order=ascending'"),
            "{cmd}"
        );
    }
}
