//! Pure domain types: identifiers, paths, field paths, values and documents.

pub mod document;
pub mod field;
pub mod ids;
pub mod path;
pub mod query;
pub mod value;
pub mod write;

pub use document::Document;
pub use field::{FieldPath, FieldPathError};
pub use ids::{CollectionId, DatabaseId, DbTarget, DocId, IdError, ProjectId};
pub use path::{AnyPath, CollectionPath, DocPath, PathError};
pub use query::{Direction, Filter, Op, OrderBy};
pub use value::{Fields, GeoPoint, GeoPointError, Timestamp, UpdateTime, Value};
pub use write::FieldOp;
