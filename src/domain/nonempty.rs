//! A vector that always holds at least one element.

use std::iter;

use thiserror::Error;

/// A list with a guaranteed first element; build it with [`TryFrom<Vec<T>>`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonEmptyVec<T>(T, Vec<T>);

/// A [`NonEmptyVec`] was built from an empty list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("expected at least one element")]
pub struct EmptyError;

impl<T> TryFrom<Vec<T>> for NonEmptyVec<T> {
    type Error = EmptyError;

    fn try_from(items: Vec<T>) -> Result<Self, EmptyError> {
        let mut items = items.into_iter();
        items
            .next()
            .map(|first| Self(first, items.collect()))
            .ok_or(EmptyError)
    }
}

impl<T> NonEmptyVec<T> {
    /// The first element.
    #[must_use]
    pub fn first(&self) -> &T {
        &self.0
    }

    /// All elements, in order.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        iter::once(&self.0).chain(&self.1)
    }

    /// The number of elements (at least 1).
    #[must_use]
    #[allow(clippy::len_without_is_empty)] // never empty by construction
    pub fn len(&self) -> usize {
        1 + self.1.len()
    }

    /// The elements as a plain vector.
    #[must_use]
    pub fn into_vec(self) -> Vec<T> {
        iter::once(self.0).chain(self.1).collect()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_and_keeps_order() {
        assert_eq!(NonEmptyVec::<u8>::try_from(vec![]), Err(EmptyError));
        let v = NonEmptyVec::try_from(vec![1, 2, 3]).unwrap();
        assert_eq!((*v.first(), v.len()), (1, 3));
        assert_eq!(v.iter().copied().collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(v.into_vec(), [1, 2, 3]);
    }
}
