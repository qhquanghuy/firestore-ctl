//! Shared fixtures for the adapter's unit tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::domain::{CollectionPath, DbTarget, DocPath, Filter, Op, Query, Scope, Value};

/// The database `project`/`database`.
pub fn target(project: &str, database: &str) -> DbTarget {
    DbTarget {
        project: project.parse().unwrap(),
        database: database.parse().unwrap(),
    }
}

/// A query over the collection at `path`, with nothing else set.
pub fn query(path: &str) -> Query {
    Query {
        scope: Scope::Collection(path.parse::<CollectionPath>().unwrap()),
        filters: Vec::new(),
        order: Vec::new(),
        limit: None,
        offset: 0,
        select: None,
    }
}

/// The document at `path`.
pub fn doc(path: &str) -> DocPath {
    path.parse().unwrap()
}

/// `field == value`.
pub fn eq(field: &str, value: Value) -> Filter {
    Filter::new(field.parse().unwrap(), Op::Eq, value).unwrap()
}
