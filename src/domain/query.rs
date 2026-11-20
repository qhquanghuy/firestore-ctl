//! Query building blocks produced by flag parsing (`--where`, `--order-by`).

use std::num::NonZeroU32;

use super::field::FieldPath;
use super::ids::CollectionId;
use super::nonempty::NonEmptyVec;
use super::path::{CollectionPath, DocPath};
use super::value::Value;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    NotIn,
    ArrayContains,
    ArrayContainsAny,
}

impl Op {
    pub const ALL: [Op; 10] = [
        Op::Eq,
        Op::Ne,
        Op::Lt,
        Op::Le,
        Op::Gt,
        Op::Ge,
        Op::In,
        Op::NotIn,
        Op::ArrayContains,
        Op::ArrayContainsAny,
    ];

    #[must_use]
    pub fn from_symbol(symbol: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.symbol() == symbol)
    }

    #[must_use]
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::In => "in",
            Self::NotIn => "not-in",
            Self::ArrayContains => "array-contains",
            Self::ArrayContainsAny => "array-contains-any",
        }
    }

    #[must_use]
    pub fn takes_list(self) -> bool {
        matches!(self, Self::In | Self::NotIn | Self::ArrayContainsAny)
    }
}

/// One `<field> <op> <value>` condition. The value is a non-empty list exactly
/// when the operator [takes a list](Op::takes_list); [`Filter::new`] enforces it.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    field: FieldPath,
    op: Op,
    value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FilterError {
    #[error("`{}` needs a list value like [a, b]", .0.symbol())]
    NeedsList(Op),
    #[error("`{}` needs a non-empty list", .0.symbol())]
    EmptyList(Op),
    #[error("`{}` does not take a list (quote it to match a string)", .0.symbol())]
    UnexpectedList(Op),
}

impl Filter {
    /// # Errors
    /// [`FilterError`] if `value` is not a list for a list operator, is a list for any other
    /// operator, or is an empty list.
    pub fn new(field: FieldPath, op: Op, value: Value) -> Result<Self, FilterError> {
        match (&value, op.takes_list()) {
            (Value::Array(items), true) if items.is_empty() => Err(FilterError::EmptyList(op)),
            (Value::Array(_), false) => Err(FilterError::UnexpectedList(op)),
            (Value::Array(_), true) | (_, false) => Ok(Self { field, op, value }),
            (_, true) => Err(FilterError::NeedsList(op)),
        }
    }

    #[must_use]
    pub fn field(&self) -> &FieldPath {
        &self.field
    }

    #[must_use]
    pub fn op(&self) -> Op {
        self.op
    }

    #[must_use]
    pub fn value(&self) -> &Value {
        &self.value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderBy {
    pub field: FieldPath,
    pub direction: Direction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Collection(CollectionPath),
    /// Every collection named `id` at any depth, optionally only below `parent`.
    Group {
        /// Restrict to collections under this document; `None` searches the whole database.
        parent: Option<DocPath>,
        id: CollectionId,
    },
}

/// A maximum result count; never zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit(NonZeroU32);

impl From<NonZeroU32> for Limit {
    fn from(n: NonZeroU32) -> Self {
        Self(n)
    }
}

impl Limit {
    #[must_use]
    pub fn get(self) -> u32 {
        self.0.get()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub scope: Scope,
    /// Conditions every result satisfies (AND).
    pub filters: Vec<Filter>,
    pub order: Vec<OrderBy>,
    pub limit: Option<Limit>,
    pub offset: u32,
    pub select: Option<Vec<FieldPath>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Aggregation {
    Count,
    Sum(FieldPath),
    Avg(FieldPath),
}

#[derive(Debug, Clone, PartialEq)]
pub struct AggregateQuery {
    pub query: Query,
    pub aggregations: NonEmptyVec<Aggregation>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn field() -> FieldPath {
        "a".parse().unwrap()
    }

    #[test]
    fn filter_new_enforces_list_rules() {
        let list = |n: usize| Value::Array(vec![Value::Null; n]);
        assert_eq!(
            Filter::new(field(), Op::In, Value::Int(1)),
            Err(FilterError::NeedsList(Op::In))
        );
        assert_eq!(
            Filter::new(field(), Op::NotIn, list(0)),
            Err(FilterError::EmptyList(Op::NotIn))
        );
        assert_eq!(
            Filter::new(field(), Op::Eq, list(1)),
            Err(FilterError::UnexpectedList(Op::Eq))
        );
        assert!(Filter::new(field(), Op::ArrayContainsAny, list(2)).is_ok());
        assert!(Filter::new(field(), Op::Lt, Value::Int(1)).is_ok());
    }
}
