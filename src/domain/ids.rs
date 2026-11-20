//! Identifier newtypes. Parsed once at the edge; always valid afterwards.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Error)]
pub enum IdError {
    #[error("{kind} must not be empty")]
    Empty { kind: &'static str },
    #[error("invalid {kind} `{input}`: {expected}")]
    Invalid {
        kind: &'static str,
        input: String,
        expected: &'static str,
    },
}

fn parse_id(
    input: &str,
    kind: &'static str,
    expected: &'static str,
    is_valid: fn(&str) -> bool,
) -> Result<String, IdError> {
    if input.is_empty() {
        Err(IdError::Empty { kind })
    } else if is_valid(input) {
        Ok(input.to_owned())
    } else {
        Err(IdError::Invalid {
            kind,
            input: input.to_owned(),
            expected,
        })
    }
}

fn is_lowercase_slug(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_database_id(s: &str) -> bool {
    s == DEFAULT_DATABASE || is_lowercase_slug(s)
}

/// No `/`, and not a Firestore-reserved name (`.`, `..`, or `__x__`).
fn is_segment(s: &str) -> bool {
    let reserved =
        s == "." || s == ".." || (s.len() >= 4 && s.starts_with("__") && s.ends_with("__"));
    !s.contains('/') && !reserved
}

const DEFAULT_DATABASE: &str = "(default)";

macro_rules! id_newtype {
    ($(#[$doc:meta])* $name:ident, $kind:literal, $expected:literal, $is_valid:path) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, IdError> {
                parse_id(s, $kind, $expected, $is_valid).map(Self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_newtype!(
    /// A Google Cloud project id (`[a-z0-9-]+`).
    ProjectId,
    "project id",
    "expected lowercase letters, digits and hyphens",
    is_lowercase_slug
);

id_newtype!(
    /// A Firestore database id: `(default)` or lowercase letters, digits and hyphens.
    DatabaseId,
    "database id",
    "expected `(default)` or lowercase letters, digits and hyphens",
    is_database_id
);

id_newtype!(
    CollectionId,
    "collection id",
    "must not contain `/`, be `.` or `..`, or match `__x__`",
    is_segment
);

id_newtype!(
    DocId,
    "document id",
    "must not contain `/`, be `.` or `..`, or match `__x__`",
    is_segment
);

impl Default for DatabaseId {
    fn default() -> Self {
        Self(DEFAULT_DATABASE.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DbTarget {
    pub project: ProjectId,
    pub database: DatabaseId,
}

impl DbTarget {
    /// Resource name of the database: `projects/P/databases/D`.
    #[must_use]
    pub fn database_path(&self) -> String {
        format!("projects/{}/databases/{}", self.project, self.database)
    }

    /// Resource name of the documents root: `projects/P/databases/D/documents`.
    #[must_use]
    pub fn documents_root(&self) -> String {
        format!("{}/documents", self.database_path())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn database_path_uses_named_db() {
        let t = DbTarget {
            project: "p".parse().unwrap(),
            database: "jobs-db".parse().unwrap(),
        };
        assert_eq!(t.database_path(), "projects/p/databases/jobs-db");
        assert_eq!(t.documents_root(), "projects/p/databases/jobs-db/documents");
    }

    #[test]
    fn default_database_is_parenthesised_default() {
        assert_eq!(DatabaseId::default().as_str(), "(default)");
        assert_eq!("(default)".parse::<DatabaseId>(), Ok(DatabaseId::default()));
    }

    #[test]
    fn project_and_database_ids_reject_bad_input() {
        assert_eq!(
            "".parse::<ProjectId>(),
            Err(IdError::Empty { kind: "project id" })
        );
        assert!(matches!(
            "My_Proj".parse::<ProjectId>(),
            Err(IdError::Invalid { .. })
        ));
        assert!(matches!(
            "a/b".parse::<DatabaseId>(),
            Err(IdError::Invalid { .. })
        ));
        assert!(matches!(
            "(other)".parse::<DatabaseId>(),
            Err(IdError::Invalid { .. })
        ));
        assert!("my-proj-123".parse::<ProjectId>().is_ok());
    }

    #[test]
    fn error_messages_mention_the_input() {
        let msg = "Bad Proj".parse::<ProjectId>().unwrap_err().to_string();
        assert!(msg.contains("Bad Proj"), "{msg}");
    }

    #[test]
    fn collection_and_doc_ids_are_single_segments() {
        assert!("users".parse::<CollectionId>().is_ok());
        assert!("a b.c".parse::<DocId>().is_ok());
        assert!("".parse::<DocId>().is_err());
        assert!("a/b".parse::<CollectionId>().is_err());
    }

    #[test]
    fn reserved_collection_and_doc_ids_are_rejected() {
        for reserved in [".", "..", "__name__", "__a__", "____"] {
            assert!(reserved.parse::<DocId>().is_err(), "{reserved}");
            assert!(reserved.parse::<CollectionId>().is_err(), "{reserved}");
        }
        for fine in ["__a", "a__", "_", "__", "___", "...", ".a", "a.b"] {
            assert!(fine.parse::<DocId>().is_ok(), "{fine}");
        }
    }
}
