//! Write building blocks: field changes from `--set` flags and whole-document writes.

use super::field::FieldPath;
use super::nonempty::NonEmptyVec;
use super::path::DocPath;
use super::value::{Fields, UpdateTime, Value};

/// One field change from a `field=value`, `field+=value` or `field-=value` flag.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldOp {
    /// Set the field to a value.
    Set(FieldPath, Value),
    /// Add the elements to the array field, skipping ones already present.
    ArrayUnion(FieldPath, Vec<Value>),
    /// Remove every occurrence of the elements from the array field.
    ArrayRemove(FieldPath, Vec<Value>),
}

/// A precondition on the document's current state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guard {
    /// Write regardless of the document's current state.
    None,
    /// Write only if the document has not changed since this `_updateTime`.
    UnchangedSince(UpdateTime),
}

/// How a whole-document write treats an existing document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetMode {
    /// Fail if the document already exists (no guard applies).
    Create,
    /// Replace the document entirely.
    Overwrite(Guard),
    /// Merge the given fields into the document, creating it if missing. With
    /// [`Guard::UnchangedSince`] a missing document fails the precondition instead.
    Merge(Guard),
}

/// Write a whole document's data.
#[derive(Debug, Clone, PartialEq)]
pub struct SetDoc {
    /// The document written.
    pub path: DocPath,
    /// The data written.
    pub data: Fields,
    /// How an existing document is treated.
    pub mode: SetMode,
}

/// Change individual fields of an existing document.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateDoc {
    /// The document changed.
    pub path: DocPath,
    /// The field changes, applied in order.
    pub ops: NonEmptyVec<FieldOp>,
    /// The precondition.
    pub guard: Guard,
}

/// Delete a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteDoc {
    /// The document deleted.
    pub path: DocPath,
    /// The precondition.
    pub guard: Guard,
}

/// Any single-document write.
#[derive(Debug, Clone, PartialEq)]
pub enum Write {
    /// [`SetDoc`]
    Set(SetDoc),
    /// [`UpdateDoc`]
    Update(UpdateDoc),
    /// [`DeleteDoc`]
    Delete(DeleteDoc),
}

impl Write {
    /// The document this write targets.
    #[must_use]
    pub fn path(&self) -> &DocPath {
        match self {
            Self::Set(w) => &w.path,
            Self::Update(w) => &w.path,
            Self::Delete(w) => &w.path,
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
}
