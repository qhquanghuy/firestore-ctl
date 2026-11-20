//! Write building blocks produced by flag parsing (`--set`).

use super::field::FieldPath;
use super::value::Value;

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
