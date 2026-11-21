#[cfg(test)]
pub(crate) mod arb;
pub mod document;
pub mod field;
pub mod ids;
pub mod index;
pub mod nonempty;
pub mod path;
pub mod query;
pub mod value;
pub mod write;

pub use document::Document;
pub use field::{FieldPath, FieldPathError};
pub use ids::{CollectionId, DatabaseId, DbTarget, DocId, IdError, ProjectId};
pub use index::{IndexAdvice, IndexField, IndexMode, IndexSpec, QueryScope, required_index};
pub use nonempty::{EmptyError, NonEmptyVec};
pub use path::{AnyPath, CollectionPath, DocPath, PathError};
pub use query::{
    AggregateQuery, Aggregation, Direction, Filter, FilterError, Limit, Op, OrderBy, Query, Scope,
};
pub use value::{
    DocResourceName, Fields, GeoPoint, GeoPointError, ResourceNameError, Timestamp, UpdateTime,
    Value,
};
pub use write::{
    DeleteDoc, FieldOp, Guard, SetDoc, SetMode, UpdateDoc, Write, WriteError, check_field_ops,
};
