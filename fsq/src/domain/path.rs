//! Collection and document paths (`users/a/orders/o1`), parsed into a typed chain.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

use super::ids::{CollectionId, DocId};

/// Why a string is not a valid collection or document path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Error)]
pub enum PathError {
    /// The path had no segments.
    #[error("path must not be empty")]
    Empty,
    /// The segment at this zero-based position was empty (e.g. `a//b`).
    #[error("path has an empty segment at position {0}")]
    EmptySegment(usize),
    /// A document path (even number of segments) was required.
    #[error("`{0}` is a collection path; expected a document path like `collection/doc`")]
    ExpectedDoc(String),
    /// A collection path (odd number of segments) was required.
    #[error("`{0}` is a document path; expected a collection path like `collection`")]
    ExpectedCollection(String),
}

/// A collection: top-level, or a subcollection of a document.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CollectionPath {
    parent: Option<Box<DocPath>>,
    id: CollectionId,
}

/// A document inside a collection.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DocPath {
    collection: CollectionPath,
    id: DocId,
}

/// Either kind of path, as typed by a user.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AnyPath {
    /// An odd number of segments: `users`, `users/a/orders`.
    Collection(CollectionPath),
    /// An even number of segments: `users/a`.
    Doc(DocPath),
}

impl CollectionPath {
    /// A collection under `parent` (a top-level collection when `None`).
    #[must_use]
    pub fn new(parent: Option<DocPath>, id: CollectionId) -> Self {
        Self {
            parent: parent.map(Box::new),
            id,
        }
    }

    /// The collection's own name (last segment).
    #[must_use]
    pub fn id(&self) -> &CollectionId {
        &self.id
    }

    /// The document owning this subcollection, if any.
    #[must_use]
    pub fn parent(&self) -> Option<&DocPath> {
        self.parent.as_deref()
    }
}

impl DocPath {
    /// The document `id` inside `collection`.
    #[must_use]
    pub fn new(collection: CollectionPath, id: DocId) -> Self {
        Self { collection, id }
    }

    /// The collection containing this document.
    #[must_use]
    pub fn collection(&self) -> &CollectionPath {
        &self.collection
    }

    /// The document's own name (last segment).
    #[must_use]
    pub fn id(&self) -> &DocId {
        &self.id
    }
}

impl fmt::Display for CollectionPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.parent {
            Some(parent) => write!(f, "{parent}/{}", self.id),
            None => write!(f, "{}", self.id),
        }
    }
}

impl fmt::Display for DocPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.collection, self.id)
    }
}

impl fmt::Display for AnyPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Collection(c) => c.fmt(f),
            Self::Doc(d) => d.fmt(f),
        }
    }
}

/// Parses one segment; an id can only fail to parse here by being empty.
fn segment<T: FromStr>(raw: &str, position: usize) -> Result<T, PathError> {
    raw.parse().map_err(|_| PathError::EmptySegment(position))
}

impl FromStr for AnyPath {
    type Err = PathError;

    /// Segments alternate `collection/doc/collection/...`; leading and
    /// trailing `/` characters are trimmed before parsing.
    fn from_str(s: &str) -> Result<Self, PathError> {
        let trimmed = s.trim_matches('/');
        if trimmed.is_empty() {
            return Err(PathError::Empty);
        }
        let segments: Vec<&str> = trimmed.split('/').collect();
        let (pairs, remainder) = segments.as_chunks::<2>();

        let last_doc = pairs.iter().enumerate().try_fold(
            None::<DocPath>,
            |parent, (i, pair)| -> Result<_, PathError> {
                let collection = CollectionPath::new(parent, segment(pair[0], 2 * i)?);
                Ok(Some(DocPath::new(collection, segment(pair[1], 2 * i + 1)?)))
            },
        )?;

        match remainder {
            [name] => Ok(Self::Collection(CollectionPath::new(
                last_doc,
                segment(name, segments.len() - 1)?,
            ))),
            _ => last_doc.map(Self::Doc).ok_or(PathError::Empty),
        }
    }
}

impl FromStr for DocPath {
    type Err = PathError;

    fn from_str(s: &str) -> Result<Self, PathError> {
        match s.parse()? {
            AnyPath::Doc(doc) => Ok(doc),
            AnyPath::Collection(_) => Err(PathError::ExpectedDoc(s.to_owned())),
        }
    }
}

impl FromStr for CollectionPath {
    type Err = PathError;

    fn from_str(s: &str) -> Result<Self, PathError> {
        match s.parse()? {
            AnyPath::Collection(collection) => Ok(collection),
            AnyPath::Doc(_) => Err(PathError::ExpectedCollection(s.to_owned())),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_doc_and_collection_paths() {
        assert!(matches!(
            "users".parse::<AnyPath>(),
            Ok(AnyPath::Collection(_))
        ));
        assert!(matches!("users/a".parse::<AnyPath>(), Ok(AnyPath::Doc(_))));
        assert!(matches!(
            "users/a/orders".parse::<AnyPath>(),
            Ok(AnyPath::Collection(_))
        ));
        assert_eq!(
            "users/a/orders/o1"
                .parse::<DocPath>()
                .map(|d| d.to_string()),
            Ok("users/a/orders/o1".into())
        );
    }

    #[test]
    fn rejects_bad_paths() {
        assert_eq!("".parse::<AnyPath>(), Err(PathError::Empty));
        assert_eq!("/".parse::<AnyPath>(), Err(PathError::Empty));
        assert!(matches!(
            "users//a".parse::<AnyPath>(),
            Err(PathError::EmptySegment(1))
        ));
        assert!(matches!(
            "users".parse::<DocPath>(),
            Err(PathError::ExpectedDoc(_))
        ));
        assert!(matches!(
            "users/a".parse::<CollectionPath>(),
            Err(PathError::ExpectedCollection(_))
        ));
        assert!("/users/a/".parse::<DocPath>().is_ok());
    }

    #[test]
    fn empty_segment_position_is_reported_in_nested_paths() {
        assert_eq!(
            "a/b/c//d".parse::<AnyPath>(),
            Err(PathError::EmptySegment(3))
        );
        assert_eq!(
            "a/b//".parse::<AnyPath>().map(|p| p.to_string()),
            Ok("a/b".into())
        );
    }

    #[test]
    fn accessors_expose_the_chain() {
        let doc: DocPath = "users/a/orders/o1".parse().unwrap();
        assert_eq!(doc.id().as_str(), "o1");
        assert_eq!(doc.collection().id().as_str(), "orders");
        let parent = doc.collection().parent().unwrap();
        assert_eq!(parent.to_string(), "users/a");
        assert!(parent.collection().parent().is_none());
    }

    #[test]
    fn display_round_trips_with_trimmed_slashes() {
        let p: AnyPath = "/users/a/orders/".parse().unwrap();
        assert_eq!(p.to_string(), "users/a/orders");
        assert_eq!(p.to_string().parse::<AnyPath>(), Ok(p));
    }

    #[test]
    fn error_messages_mention_the_input() {
        let msg = "users".parse::<DocPath>().unwrap_err().to_string();
        assert!(msg.contains("users"), "{msg}");
    }
}
