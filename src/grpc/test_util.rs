use crate::domain::{CollectionPath, DbTarget, DocPath, Filter, Op, Query, Scope, Value};

pub(super) fn target(project: &str, database: &str) -> DbTarget {
    DbTarget {
        project: project.parse().unwrap(),
        database: database.parse().unwrap(),
    }
}

pub(super) fn query(path: &str) -> Query {
    Query {
        scope: Scope::Collection(path.parse::<CollectionPath>().unwrap()),
        filters: Vec::new(),
        order: Vec::new(),
        limit: None,
        offset: 0,
        select: None,
    }
}

pub(super) fn doc(path: &str) -> DocPath {
    path.parse().unwrap()
}

pub(super) fn eq(field: &str, value: Value) -> Filter {
    Filter::new(field.parse().unwrap(), Op::Eq, value).unwrap()
}
