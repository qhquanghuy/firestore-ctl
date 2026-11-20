//! Write building blocks: field changes from `--set` flags and whole-document writes.

use itertools::Itertools;
use thiserror::Error;

use super::field::FieldPath;
use super::nonempty::NonEmptyVec;
use super::path::DocPath;
use super::value::{Fields, UpdateTime, Value};

/// One field change from a `field=value`, `field+=value` or `field-=value` flag.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldOp {
    Set(FieldPath, Value),
    /// Add the elements to the array field, skipping ones already present.
    ArrayUnion(FieldPath, Vec<Value>),
    /// Remove every occurrence of the elements from the array field.
    ArrayRemove(FieldPath, Vec<Value>),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WriteError {
    #[error(
        "conflicting changes to `{first}` and `{second}`: each field can be changed once, and not together with a field inside it"
    )]
    Overlap { first: FieldPath, second: FieldPath },
}

impl FieldOp {
    #[must_use]
    pub fn path(&self) -> &FieldPath {
        match self {
            Self::Set(p, _) | Self::ArrayUnion(p, _) | Self::ArrayRemove(p, _) => p,
        }
    }
}

/// Firestore rejects a field changed twice (a set and an array change included) and a map
/// changed together with a field inside it (`a` with `a.b`).
///
/// # Errors
/// [`WriteError::Overlap`] naming the first conflicting pair.
pub fn check_field_ops(ops: &NonEmptyVec<FieldOp>) -> Result<(), WriteError> {
    let overlaps = |a: &FieldPath, b: &FieldPath| {
        a.segments().starts_with(b.segments()) || b.segments().starts_with(a.segments())
    };
    ops.iter()
        .map(FieldOp::path)
        .collect_vec()
        .into_iter()
        .array_combinations::<2>()
        .find(|[a, b]| overlaps(a, b))
        .map_or(Ok(()), |[a, b]| {
            Err(WriteError::Overlap {
                first: a.clone(),
                second: b.clone(),
            })
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guard {
    None,
    UnchangedSince(UpdateTime),
}

impl Guard {
    #[must_use]
    pub fn unchanged_since(&self) -> Option<&UpdateTime> {
        match self {
            Self::None => None,
            Self::UnchangedSince(t) => Some(t),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetMode {
    Create,
    Overwrite(Guard),
    /// Merge the given fields into the document, creating it if missing. With
    /// [`Guard::UnchangedSince`] a missing document fails the precondition instead.
    Merge(Guard),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SetDoc {
    pub path: DocPath,
    pub data: Fields,
    pub mode: SetMode,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UpdateDoc {
    pub path: DocPath,
    pub ops: NonEmptyVec<FieldOp>,
    pub guard: Guard,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteDoc {
    pub path: DocPath,
    pub guard: Guard,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Write {
    Set(SetDoc),
    Update(UpdateDoc),
    Delete(DeleteDoc),
}

impl Write {
    #[must_use]
    pub fn path(&self) -> &DocPath {
        match self {
            Self::Set(w) => &w.path,
            Self::Update(w) => &w.path,
            Self::Delete(w) => &w.path,
        }
    }

    /// The update time the document must still have, if guarded. `Create` has no guard (it
    /// requires the document to be absent).
    #[must_use]
    pub fn guard(&self) -> Option<&UpdateTime> {
        match self {
            Self::Set(SetDoc { mode, .. }) => match mode {
                SetMode::Create => None,
                SetMode::Overwrite(g) | SetMode::Merge(g) => g.unchanged_since(),
            },
            Self::Update(w) => w.guard.unchanged_since(),
            Self::Delete(w) => w.guard.unchanged_since(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn write_path_is_the_target_document() {
        let path: DocPath = "users/u1".parse().unwrap();
        let delete = Write::Delete(DeleteDoc {
            path: path.clone(),
            guard: Guard::None,
        });
        let set = Write::Set(SetDoc {
            path: path.clone(),
            data: Fields::new(),
            mode: SetMode::Create,
        });
        assert_eq!((delete.path(), set.path()), (&path, &path));
    }

    #[test]
    fn guard_is_the_unchanged_since_time_of_any_write_kind() {
        let path: DocPath = "users/u1".parse().unwrap();
        let t: UpdateTime = "2026-01-05T10:00:00.5Z".parse().unwrap();
        let g = Guard::UnchangedSince(t);
        let set = |mode| {
            Write::Set(SetDoc {
                path: path.clone(),
                data: Fields::new(),
                mode,
            })
        };
        let writes = [
            set(SetMode::Overwrite(g)),
            set(SetMode::Merge(g)),
            Write::Delete(DeleteDoc {
                path: path.clone(),
                guard: g,
            }),
            Write::Update(UpdateDoc {
                path: path.clone(),
                ops: NonEmptyVec::try_from(vec![FieldOp::Set("a".parse().unwrap(), Value::Null)])
                    .unwrap(),
                guard: g,
            }),
        ];
        assert!(writes.iter().all(|w| w.guard() == Some(&t)));
        let unguarded = [
            set(SetMode::Create),
            set(SetMode::Overwrite(Guard::None)),
            Write::Delete(DeleteDoc {
                path,
                guard: Guard::None,
            }),
        ];
        assert!(unguarded.iter().all(|w| w.guard().is_none()));
    }

    fn ops(ops: Vec<FieldOp>) -> NonEmptyVec<FieldOp> {
        NonEmptyVec::try_from(ops).unwrap()
    }

    #[test]
    fn field_ops_that_overlap_are_rejected() {
        let path = |s: &str| s.parse::<FieldPath>().unwrap();
        let set = |p: &str| FieldOp::Set(path(p), Value::Int(1));
        let union = |p: &str| FieldOp::ArrayUnion(path(p), vec![Value::Int(1)]);
        let remove = |p: &str| FieldOp::ArrayRemove(path(p), vec![Value::Int(1)]);
        for conflict in [
            vec![set("a"), set("a.b")],
            vec![set("a.b"), set("a")],
            vec![set("a"), set("a")],
            vec![set("tags"), union("tags")],
            vec![union("tags"), remove("tags")],
        ] {
            assert!(
                matches!(
                    check_field_ops(&ops(conflict.clone())),
                    Err(WriteError::Overlap { .. })
                ),
                "{conflict:?}"
            );
        }
        let err = check_field_ops(&ops(vec![set("x"), set("a"), set("a.b")])).unwrap_err();
        assert_eq!(
            err,
            WriteError::Overlap {
                first: path("a"),
                second: path("a.b")
            }
        );
        assert!(err.to_string().contains("`a` and `a.b`"));
    }

    #[test]
    fn siblings_and_lookalikes_do_not_overlap() {
        let set = |p: &str| FieldOp::Set(p.parse().unwrap(), Value::Int(1));
        let fine = ops(vec![
            set("a.b"),
            set("a.c"),
            set("ab"),
            set("`a.b`"),
            set("a2"),
        ]);
        assert_eq!(check_field_ops(&fine), Ok(()));
    }
}
