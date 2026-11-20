//! An in-memory [`Firestore`] for flow tests. Follows the write contract documented on
//! `Firestore::write` exactly. Models `==` filters on top-level fields, `limit` and
//! `count`; anything else (order, offset, select, other operators, nested fields,
//! sum/avg, nested-map merge) fails with `GatewayError::Other("fake: unsupported ...")`.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

use fsq::domain::{
    AggregateQuery, Aggregation, CollectionId, DbTarget, DocPath, Document, FieldOp, Fields, Guard,
    Op, Query, Scope, SetMode, Timestamp, UpdateTime, Value, Write,
};
use fsq::policy::WriteAccess;
use fsq::port::{AggregateResult, Firestore, GatewayError, IndexInfo, Written};

/// The database the fake pretends to be connected to unless told otherwise.
pub fn default_target() -> DbTarget {
    DbTarget {
        project: "p".parse().expect("valid project"),
        database: "d".parse().expect("valid database"),
    }
}

/// An in-memory Firestore for one database.
#[derive(Debug)]
pub struct FakeFirestore {
    target: DbTarget,
    docs: RefCell<BTreeMap<DocPath, Document>>,
    calls: RefCell<Vec<&'static str>>,
    clock: Cell<i64>,
    index_missing: bool,
}

impl Default for FakeFirestore {
    fn default() -> Self {
        Self::for_target(default_target())
    }
}

impl FakeFirestore {
    /// An empty fake connected to `target`.
    pub fn for_target(target: DbTarget) -> Self {
        Self {
            target,
            docs: RefCell::default(),
            calls: RefCell::default(),
            clock: Cell::new(1_000),
            index_missing: false,
        }
    }

    /// A fake on the default target holding `doc`.
    pub fn with(doc: Document) -> Self {
        let fake = Self::default();
        fake.docs.borrow_mut().insert(doc.path.clone(), doc);
        fake
    }

    /// Makes every query and aggregate fail with a missing-index error.
    pub fn without_index(self) -> Self {
        Self {
            index_missing: true,
            ..self
        }
    }

    /// The port methods called so far, in order.
    pub fn calls(&self) -> Vec<&'static str> {
        self.calls.borrow().clone()
    }

    /// The stored document at `p`, if any.
    pub fn stored(&self, p: &DocPath) -> Option<Document> {
        self.docs.borrow().get(p).cloned()
    }

    fn record(&self, call: &'static str) {
        self.calls.borrow_mut().push(call);
    }

    fn tick(&self) -> UpdateTime {
        let seconds = self.clock.get() + 1;
        self.clock.set(seconds);
        UpdateTime::new(Timestamp::from_unix(seconds, 0).expect("in-range timestamp"))
    }

    /// The documents matching `q`'s scope and `==` filters, up to its limit (no call
    /// recorded). Anything the fake does not model is an error, never silently ignored.
    fn matching(&self, q: &Query) -> Result<Vec<Document>, GatewayError> {
        let unsupported =
            |what: &str| Err(GatewayError::Other(format!("fake: unsupported {what}")));
        if !q.order.is_empty() {
            return unsupported("order-by");
        }
        if q.offset > 0 {
            return unsupported("offset");
        }
        if q.select.is_some() {
            return unsupported("select");
        }
        if q.filters.iter().any(|f| f.op() != Op::Eq) {
            return unsupported("filter operator (only == is modelled)");
        }
        if q.filters.iter().any(|f| f.field().segments().len() != 1) {
            return unsupported("nested field filter");
        }
        Ok(self
            .docs
            .borrow()
            .values()
            .filter(|d| in_scope(&q.scope, d))
            .filter(|d| {
                q.filters
                    .iter()
                    .all(|f| d.data.get(&f.field().segments()[0]) == Some(f.value()))
            })
            .take(q.limit.map_or(usize::MAX, |l| l.get() as usize))
            .cloned()
            .collect())
    }

    fn missing_index() -> GatewayError {
        GatewayError::MissingIndex {
            console_url: Some("https://console.example/index".to_owned()),
        }
    }
}

/// What a write requires of the stored document when it carries no `UnchangedSince`
/// guard (Firestore cannot combine `exists` with `update_time`).
#[derive(Clone, Copy)]
enum Unguarded {
    /// No precondition (upsert).
    Anything,
    /// `exists = false`: otherwise `AlreadyExists`.
    MustBeAbsent,
    /// `exists = true`: otherwise `NotFound`.
    MustExist,
}

/// The write contract documented on [`Firestore::write`]: `UnchangedSince(t)` demands
/// the document exists with update time `t` (`PreconditionFailed` otherwise); with
/// [`Guard::None`] the write's own `unguarded` requirement applies.
fn check_precondition(
    path: &DocPath,
    existing: Option<&Document>,
    unguarded: Unguarded,
    guard: Guard,
) -> Result<(), GatewayError> {
    match (guard, unguarded, existing) {
        (Guard::UnchangedSince(t), _, Some(d)) if d.update_time == t => Ok(()),
        (Guard::UnchangedSince(_), _, _) => Err(GatewayError::PreconditionFailed(path.clone())),
        (Guard::None, Unguarded::MustBeAbsent, Some(_)) => {
            Err(GatewayError::AlreadyExists(path.clone()))
        }
        (Guard::None, Unguarded::MustExist, None) => Err(GatewayError::NotFound(path.clone())),
        (Guard::None, _, _) => Ok(()),
    }
}

/// Top-level merge of `new` over the stored fields. Shallow: nested maps replace
/// rather than merge, so the fake refuses them instead of diverging from Firestore.
fn shallow_merge(existing: Option<Document>, new: &Fields) -> Result<Fields, GatewayError> {
    if new.values().any(|v| matches!(v, Value::Map(_))) {
        return Err(GatewayError::Other(
            "fake: merging nested maps is not supported".to_owned(),
        ));
    }
    Ok(existing
        .map(|d| d.data)
        .unwrap_or_default()
        .into_iter()
        .chain(new.clone())
        .collect())
}

/// Applies one field change to a top-level field.
fn apply_op(mut fields: Fields, op: &FieldOp) -> Result<Fields, GatewayError> {
    let (field, new) = match op {
        FieldOp::Set(field, v) => (field, v.clone()),
        FieldOp::ArrayUnion(field, add) => {
            let old = current_items(&fields, field.segments());
            let merged = add.iter().fold(old, |mut items, v| {
                if !items.contains(v) {
                    items.push(v.clone());
                }
                items
            });
            (field, Value::Array(merged))
        }
        FieldOp::ArrayRemove(field, gone) => {
            let kept = current_items(&fields, field.segments())
                .into_iter()
                .filter(|v| !gone.contains(v))
                .collect();
            (field, Value::Array(kept))
        }
    };
    let [key] = field.segments() else {
        return Err(GatewayError::Other(
            "fake: nested field paths are not supported".to_owned(),
        ));
    };
    fields.insert(key.clone(), new);
    Ok(fields)
}

fn current_items(fields: &Fields, segments: &[String]) -> Vec<Value> {
    match segments {
        [key] => match fields.get(key) {
            Some(Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn in_scope(scope: &Scope, d: &Document) -> bool {
    match scope {
        Scope::Collection(c) => d.path.collection() == c,
        Scope::Group { parent, id } => {
            d.path.collection().id() == id
                && parent
                    .as_ref()
                    .is_none_or(|p| d.path.collection().parent() == Some(p))
        }
    }
}

// The fake is synchronous inside; the trait is async because the real adapter is.
#[allow(clippy::unused_async_trait_impl)]
impl Firestore for FakeFirestore {
    async fn get(&self, p: &DocPath) -> Result<Option<Document>, GatewayError> {
        self.record("get");
        Ok(self.stored(p))
    }

    async fn query(&self, q: &Query) -> Result<Vec<Document>, GatewayError> {
        self.record("query");
        if self.index_missing {
            return Err(Self::missing_index());
        }
        self.matching(q)
    }

    async fn aggregate(&self, q: &AggregateQuery) -> Result<AggregateResult, GatewayError> {
        self.record("aggregate");
        if self.index_missing {
            return Err(Self::missing_index());
        }
        if q.aggregations.iter().any(|a| *a != Aggregation::Count) {
            return Err(GatewayError::Other(
                "fake: unsupported aggregation (only count is modelled)".to_owned(),
            ));
        }
        let count = self.matching(&q.query)?.len();
        Ok(AggregateResult::new(
            q.aggregations
                .iter()
                .map(|a| {
                    (
                        a.clone(),
                        Value::Int(i64::try_from(count).unwrap_or(i64::MAX)),
                    )
                })
                .collect(),
        ))
    }

    async fn list_collections(
        &self,
        parent: Option<&DocPath>,
    ) -> Result<Vec<CollectionId>, GatewayError> {
        self.record("list_collections");
        Ok(self
            .docs
            .borrow()
            .keys()
            .map(DocPath::collection)
            .filter(|c| c.parent() == parent)
            .map(|c| c.id().clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect())
    }

    async fn list_indexes(&self, _c: &CollectionId) -> Result<Vec<IndexInfo>, GatewayError> {
        self.record("list_indexes");
        Ok(Vec::new())
    }

    async fn write(&self, w: &Write, access: &WriteAccess) -> Result<Written, GatewayError> {
        self.record("write");
        if access.target() != &self.target {
            return Err(GatewayError::Other(format!(
                "write access for {}/{} used on {}/{}",
                access.target().project,
                access.target().database,
                self.target.project,
                self.target.database
            )));
        }
        let path = w.path();
        let existing = self.stored(path);
        let new_data: Option<Fields> = match w {
            Write::Set(s) => {
                let (unguarded, guard) = match s.mode {
                    SetMode::Create => (Unguarded::MustBeAbsent, Guard::None),
                    SetMode::Overwrite(g) | SetMode::Merge(g) => (Unguarded::Anything, g),
                };
                check_precondition(path, existing.as_ref(), unguarded, guard)?;
                Some(match s.mode {
                    SetMode::Merge(_) => shallow_merge(existing, &s.data)?,
                    SetMode::Create | SetMode::Overwrite(_) => s.data.clone(),
                })
            }
            Write::Update(u) => {
                check_precondition(path, existing.as_ref(), Unguarded::MustExist, u.guard)?;
                let current = existing.map(|d| d.data).unwrap_or_default();
                Some(u.ops.iter().try_fold(current, apply_op)?)
            }
            Write::Delete(d) => {
                check_precondition(path, existing.as_ref(), Unguarded::MustExist, d.guard)?;
                None
            }
        };
        Ok(if let Some(data) = new_data {
            let update_time = self.tick();
            self.docs.borrow_mut().insert(
                path.clone(),
                Document {
                    path: path.clone(),
                    update_time,
                    data,
                },
            );
            Written::Updated(update_time)
        } else {
            self.docs.borrow_mut().remove(path);
            Written::Deleted
        })
    }
}
