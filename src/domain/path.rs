use std::fmt;
use std::str::FromStr;

use thiserror::Error;

use super::ids::{CollectionId, DocId, IdError};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Error)]
pub enum PathError {
    #[error("path must not be empty")]
    Empty,
    #[error("path `{input}` has an empty segment at position {position}")]
    EmptySegment { input: String, position: usize },
    #[error("path `{input}` has an invalid segment at position {position}: {source}")]
    InvalidSegment {
        input: String,
        position: usize,
        source: IdError,
    },
    #[error("`{0}` is a collection path; expected a document path like `collection/doc`")]
    ExpectedDoc(String),
    #[error("`{0}` is a document path; expected a collection path like `collection`")]
    ExpectedCollection(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CollectionPath {
    parent: Option<Box<DocPath>>,
    id: CollectionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocPath {
    collection: CollectionPath,
    id: DocId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AnyPath {
    Collection(CollectionPath),
    Doc(DocPath),
}

impl CollectionPath {
    #[must_use]
    pub fn new(parent: Option<DocPath>, id: CollectionId) -> Self {
        Self {
            parent: parent.map(Box::new),
            id,
        }
    }

    #[must_use]
    pub fn id(&self) -> &CollectionId {
        &self.id
    }

    #[must_use]
    pub fn parent(&self) -> Option<&DocPath> {
        self.parent.as_deref()
    }
}

impl DocPath {
    #[must_use]
    pub fn new(collection: CollectionPath, id: DocId) -> Self {
        Self { collection, id }
    }

    #[must_use]
    pub fn collection(&self) -> &CollectionPath {
        &self.collection
    }

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

fn segment<T: FromStr<Err = IdError>>(
    raw: &str,
    position: usize,
    input: &str,
) -> Result<T, PathError> {
    if raw.is_empty() {
        return Err(PathError::EmptySegment {
            input: input.to_owned(),
            position,
        });
    }
    raw.parse().map_err(|source| PathError::InvalidSegment {
        input: input.to_owned(),
        position,
        source,
    })
}

impl FromStr for AnyPath {
    type Err = PathError;

    fn from_str(s: &str) -> Result<Self, PathError> {
        let trimmed = s.strip_prefix('/').unwrap_or(s);
        let trimmed = trimmed.strip_suffix('/').unwrap_or(trimmed);
        if trimmed.is_empty() {
            return Err(PathError::Empty);
        }
        let segments: Vec<&str> = trimmed.split('/').collect();
        let (pairs, remainder) = segments.as_chunks::<2>();

        let last_doc = pairs.iter().enumerate().try_fold(
            None::<DocPath>,
            |parent, (i, pair)| -> Result<_, PathError> {
                let collection = CollectionPath::new(parent, segment(pair[0], 2 * i, s)?);
                Ok(Some(DocPath::new(
                    collection,
                    segment(pair[1], 2 * i + 1, s)?,
                )))
            },
        )?;

        match remainder {
            [name] => Ok(Self::Collection(CollectionPath::new(
                last_doc,
                segment(name, segments.len() - 1, s)?,
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
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::domain::arb;

    #[test]
    fn rejects_bad_paths() {
        assert_eq!("".parse::<AnyPath>(), Err(PathError::Empty));
        assert_eq!("/".parse::<AnyPath>(), Err(PathError::Empty));
        assert!(matches!(
            "users//a".parse::<AnyPath>(),
            Err(PathError::EmptySegment { position: 1, .. })
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
    fn at_most_one_leading_and_trailing_slash_is_removed() {
        assert_eq!(
            "a/b/".parse::<AnyPath>().map(|p| p.to_string()),
            Ok("a/b".into())
        );
        assert_eq!(
            "users/".parse::<AnyPath>().map(|p| p.to_string()),
            Ok("users".into())
        );
        assert!(matches!(
            "users//".parse::<AnyPath>(),
            Err(PathError::EmptySegment { position: 1, .. })
        ));
        assert!(matches!(
            "//users".parse::<AnyPath>(),
            Err(PathError::EmptySegment { position: 0, .. })
        ));
        assert!(matches!(
            "a/b//".parse::<AnyPath>(),
            Err(PathError::EmptySegment { position: 2, .. })
        ));
    }

    #[test]
    fn empty_segment_position_is_reported_in_nested_paths() {
        assert!(matches!(
            "a/b/c//d".parse::<AnyPath>(),
            Err(PathError::EmptySegment { position: 3, .. })
        ));
    }

    #[test]
    fn invalid_segments_carry_position_and_cause() {
        let err = "users/a/orders/..".parse::<AnyPath>().unwrap_err();
        assert!(matches!(
            err,
            PathError::InvalidSegment {
                position: 3,
                source: IdError::Invalid { .. },
                ..
            }
        ));
        assert!(err.to_string().contains("users/a/orders/.."), "{err}");
        assert!(matches!(
            "__x__/a".parse::<AnyPath>(),
            Err(PathError::InvalidSegment { position: 0, .. })
        ));
    }

    #[test]
    fn empty_segment_message_mentions_the_input() {
        let msg = "/users//a".parse::<AnyPath>().unwrap_err().to_string();
        assert!(msg.contains("/users//a"), "{msg}");
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

    proptest! {
        #![proptest_config(arb::config())]

        #[test]
        fn doc_path_display_round_trips(p in arb::doc_path()) {
            prop_assert_eq!(p.to_string().parse::<DocPath>(), Ok(p));
        }

        #[test]
        fn collection_path_display_round_trips(p in arb::collection_path()) {
            prop_assert_eq!(p.to_string().parse::<CollectionPath>(), Ok(p));
        }

        #[test]
        fn segment_count_decides_the_kind(d in arb::doc_path(), c in arb::collection_path()) {
            prop_assert_eq!(d.to_string().parse::<AnyPath>(), Ok(AnyPath::Doc(d.clone())));
            prop_assert_eq!(c.to_string().parse::<AnyPath>(), Ok(AnyPath::Collection(c.clone())));
            prop_assert_eq!(
                d.to_string().parse::<CollectionPath>(),
                Err(PathError::ExpectedCollection(d.to_string()))
            );
            prop_assert_eq!(
                c.to_string().parse::<DocPath>(),
                Err(PathError::ExpectedDoc(c.to_string()))
            );
        }

        #[test]
        fn one_leading_and_one_trailing_slash_are_trimmed(
            p in arb::doc_path(),
            leading in any::<bool>(),
            trailing in any::<bool>(),
        ) {
            let text = format!(
                "{}{p}{}",
                if leading { "/" } else { "" },
                if trailing { "/" } else { "" },
            );
            prop_assert_eq!(text.parse::<DocPath>(), Ok(p));
        }

        #[test]
        fn parsing_never_panics_and_a_parsed_path_reprints_stably(s in arb::text()) {
            if let Ok(p) = s.parse::<AnyPath>() {
                prop_assert_eq!(p.to_string().parse::<AnyPath>(), Ok(p));
            }
        }
    }
}
