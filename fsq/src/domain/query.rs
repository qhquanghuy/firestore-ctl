//! Query building blocks produced by flag parsing (`--where`, `--order-by`).

use super::field::FieldPath;
use super::value::Value;
use thiserror::Error;

/// A comparison operator in a `--where` filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `in` (value is a list)
    In,
    /// `not-in` (value is a list)
    NotIn,
    /// `array-contains`
    ArrayContains,
    /// `array-contains-any` (value is a list)
    ArrayContainsAny,
}

impl Op {
    /// Every operator.
    const ALL: [Op; 10] = [
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

    /// The operator spelled `symbol` on the command line, if any.
    #[must_use]
    pub fn from_symbol(symbol: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.symbol() == symbol)
    }

    /// The operator's command-line spelling.
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

    /// Whether the operator compares against a list of values.
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

/// Why a field, operator and value do not form a valid [`Filter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FilterError {
    /// The operator compares against a list but the value is not one.
    #[error("`{}` needs a list value like [a, b]", .0.symbol())]
    NeedsList(Op),
    /// The operator compares against a list but the list is empty.
    #[error("`{}` needs a non-empty list", .0.symbol())]
    EmptyList(Op),
    /// The operator takes a single value but the value is a list.
    #[error("`{}` does not take a list (quote it to match a string)", .0.symbol())]
    UnexpectedList(Op),
}

impl Filter {
    /// # Errors
    /// [`FilterError`] if `value` is not a list for `in`/`not-in`/`array-contains-any`,
    /// is a list for any other operator, or is an empty list.
    pub fn new(field: FieldPath, op: Op, value: Value) -> Result<Self, FilterError> {
        match (&value, op.takes_list()) {
            (Value::Array(items), true) if items.is_empty() => Err(FilterError::EmptyList(op)),
            (Value::Array(_), false) => Err(FilterError::UnexpectedList(op)),
            (Value::Array(_), true) | (_, false) => Ok(Self { field, op, value }),
            (_, true) => Err(FilterError::NeedsList(op)),
        }
    }

    /// The field compared.
    #[must_use]
    pub fn field(&self) -> &FieldPath {
        &self.field
    }

    /// The comparison.
    #[must_use]
    pub fn op(&self) -> Op {
        self.op
    }

    /// The right-hand side.
    #[must_use]
    pub fn value(&self) -> &Value {
        &self.value
    }
}

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Smallest first.
    Asc,
    /// Largest first.
    Desc,
}

/// One `--order-by` term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderBy {
    /// The field sorted on.
    pub field: FieldPath,
    /// The sort direction.
    pub direction: Direction,
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
