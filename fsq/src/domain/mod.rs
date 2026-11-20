//! Pure domain types: identifiers, document paths and field paths.

pub mod field;
pub mod ids;
pub mod path;

pub use field::{FieldPath, FieldPathError};
pub use ids::{CollectionId, DatabaseId, DbTarget, DocId, IdError, ProjectId};
pub use path::{AnyPath, CollectionPath, DocPath, PathError};
