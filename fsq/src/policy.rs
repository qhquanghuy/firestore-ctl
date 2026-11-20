//! The write gate: writes are possible only on databases the human listed in
//! `FSQ_WRITABLE`. [`WriteAccess`] can only be obtained from [`WritePolicy::authorize`],
//! so any code that writes must take one.

use std::str::FromStr;

use itertools::Itertools;
use thiserror::Error;

use crate::domain::{DatabaseId, DbTarget, IdError, ProjectId};

/// One `FSQ_WRITABLE` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WritableEntry {
    /// `database`: that database in any project.
    AnyProject(DatabaseId),
    /// `project/database`: exactly that database.
    Exact(DbTarget),
}

impl WritableEntry {
    fn matches(&self, t: &DbTarget) -> bool {
        match self {
            Self::AnyProject(database) => *database == t.database,
            Self::Exact(target) => target == t,
        }
    }
}

/// Why `FSQ_WRITABLE` could not be parsed. Meant for the human who set it.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PolicyError {
    /// An entry used a wildcard.
    #[error("FSQ_WRITABLE entry `{0}`: wildcards are not allowed; list databases explicitly")]
    Wildcard(String),
    /// A bare `(default)` would match every project's default database.
    #[error(
        "FSQ_WRITABLE entry `(default)`: name the project, e.g. `my-project/(default)`; \
         a bare `(default)` would allow writes to every project's default database"
    )]
    BareDefault,
    /// An entry was not `database` or `project/database`.
    #[error("FSQ_WRITABLE entry `{entry}`: {reason}")]
    Invalid {
        /// The offending entry.
        entry: String,
        /// Which identifier was invalid and why.
        reason: IdError,
    },
}

impl FromStr for WritableEntry {
    type Err = PolicyError;

    fn from_str(entry: &str) -> Result<Self, PolicyError> {
        let invalid = |reason| PolicyError::Invalid {
            entry: entry.to_owned(),
            reason,
        };
        if entry.contains('*') {
            return Err(PolicyError::Wildcard(entry.to_owned()));
        }
        if entry == "(default)" {
            return Err(PolicyError::BareDefault);
        }
        match entry.split_once('/') {
            Some((project, database)) => Ok(Self::Exact(DbTarget {
                project: project.parse::<ProjectId>().map_err(invalid)?,
                database: database.parse::<DatabaseId>().map_err(invalid)?,
            })),
            None => entry.parse().map(Self::AnyProject).map_err(invalid),
        }
    }
}

/// The databases writes are allowed on. Empty means read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WritePolicy(Vec<WritableEntry>);

impl WritePolicy {
    /// Parses the value of `FSQ_WRITABLE`: comma-separated `database` or
    /// `project/database` entries; whitespace around entries and empty entries are
    /// ignored. Unset or empty yields a read-only policy.
    ///
    /// # Errors
    /// [`PolicyError`] for a wildcard or an invalid project or database id.
    pub fn from_env_value(v: Option<&str>) -> Result<Self, PolicyError> {
        v.unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(str::parse)
            .try_collect()
            .map(Self)
    }

    /// Grants write access to `t` if an entry matches it.
    ///
    /// # Errors
    /// [`WriteDenied`] when no entry matches (including when the policy is empty).
    pub fn authorize(&self, t: &DbTarget) -> Result<WriteAccess, WriteDenied> {
        self.0
            .iter()
            .any(|entry| entry.matches(t))
            .then(|| WriteAccess {
                target: t.clone(),
                _sealed: (),
            })
            .ok_or_else(|| WriteDenied(t.clone()))
    }
}

/// Proof that writes to one database are allowed. Only [`WritePolicy::authorize`]
/// creates one.
#[derive(Debug, Clone)]
pub struct WriteAccess {
    target: DbTarget,
    _sealed: (),
}

impl WriteAccess {
    /// The database this grant covers.
    #[must_use]
    pub fn target(&self) -> &DbTarget {
        &self.target
    }
}

/// A write was refused because the database is not writable.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("writes are disabled for database {}/{}", .0.project, .0.database)]
pub struct WriteDenied(DbTarget);

impl WriteDenied {
    /// The database that refused the write.
    #[must_use]
    pub fn target(&self) -> &DbTarget {
        &self.0
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn target(project: &str, database: &str) -> DbTarget {
        DbTarget {
            project: project.parse().unwrap(),
            database: database.parse().unwrap(),
        }
    }

    #[test]
    fn policy_matches_db_name_or_exact() {
        let p = WritePolicy::from_env_value(Some("jobs-db, dev/(default)")).unwrap();
        assert!(p.authorize(&target("any", "jobs-db")).is_ok());
        assert!(p.authorize(&target("dev", "(default)")).is_ok());
        assert!(p.authorize(&target("prod", "(default)")).is_err());
        assert!(p.authorize(&target("p", "app-db")).is_err());
    }

    #[test]
    fn policy_empty_or_unset_is_read_only() {
        for v in [None, Some(""), Some("  "), Some(" , ,")] {
            assert!(
                WritePolicy::from_env_value(v)
                    .unwrap()
                    .authorize(&target("p", "d"))
                    .is_err()
            );
        }
    }

    #[test]
    fn policy_rejects_wildcard() {
        for v in ["*", "dev/*", "a,*"] {
            assert!(WritePolicy::from_env_value(Some(v)).is_err(), "{v}");
        }
    }

    #[test]
    fn policy_rejects_bare_default_database() {
        assert_eq!(
            WritePolicy::from_env_value(Some("a, (default)")),
            Err(PolicyError::BareDefault)
        );
        assert!(WritePolicy::from_env_value(Some("p/(default)")).is_ok());
    }

    #[test]
    fn policy_rejects_invalid_ids() {
        for v in ["Bad_Db", "p/", "/d", "a/b/c", "P/d"] {
            assert!(WritePolicy::from_env_value(Some(v)).is_err(), "{v}");
        }
    }

    #[test]
    fn denial_message_does_not_leak_mechanism() {
        let e = WritePolicy::from_env_value(None)
            .unwrap()
            .authorize(&target("p", "d"))
            .unwrap_err();
        assert_eq!(e.to_string(), "writes are disabled for database p/d");
    }

    #[test]
    fn grant_names_the_authorized_target() {
        let p = WritePolicy::from_env_value(Some("d")).unwrap();
        let t = target("p", "d");
        assert_eq!(p.authorize(&t).unwrap().target(), &t);
    }
}
