//! Query building blocks produced by flag parsing (`--where`, `--order-by`).

use super::field::FieldPath;
use super::value::Value;

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

/// One `<field> <op> <value>` condition.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    /// The field compared.
    pub field: FieldPath,
    /// The comparison.
    pub op: Op,
    /// The right-hand side (an array exactly when [`Op::takes_list`]).
    pub value: Value,
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
