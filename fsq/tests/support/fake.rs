//! An in-memory [`Firestore`] for flow tests. Honours `SetMode`/`Guard` semantics
//! and evaluates `==` filters only; everything else is out of scope for the fake.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use fsq::domain::{
    AggregateQuery, CollectionId, DbTarget, DocPath, Document, FieldOp, Fields, Guard, Op, Query,
    Scope, SetMode, Timestamp, UpdateTime, Value, Write,
};
use fsq::policy::WriteAccess;
use fsq::port::{AggregateResult, Firestore, GatewayError, IndexInfo};

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

    /// The documents matching `q`'s scope and `==` filters (no call recorded).
    fn matching(&self, q: &Query) -> Result<Vec<Document>, GatewayError> {
        if q.filters.iter().any(|f| f.op() != Op::Eq) {
            return Err(GatewayError::Other(
                "fake: only == filters are supported".to_owned(),
            ));
        }
        Ok(self
            .docs
            .borrow()
            .values()
            .filter(|d| in_scope(&q.scope, d))
            .filter(|d| {
                q.filters.iter().all(|f| match f.field().segments() {
                    [key] => d.data.get(key) == Some(f.value()),
                    _ => false,
                })
            })
            .cloned()
            .collect())
    }

    fn missing_index() -> GatewayError {
        GatewayError::MissingIndex {
            console_url: Some("https://console.example/index".to_owned()),
        }
    }
}

/// Fails with `PreconditionFailed` unless `guard` holds for the stored `existing`.
fn check_guard(
    path: &DocPath,
    existing: Option<&Document>,
    guard: Guard,
) -> Result<(), GatewayError> {
    match (guard, existing) {
        (Guard::None, _) => Ok(()),
        (Guard::UnchangedSince(t), Some(d)) if d.update_time == t => Ok(()),
        (Guard::UnchangedSince(_), _) => Err(GatewayError::PreconditionFailed(path.clone())),
    }
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
        let mut ids: Vec<CollectionId> = self
            .docs
            .borrow()
            .keys()
            .map(DocPath::collection)
            .filter(|c| c.parent() == parent)
            .map(|c| c.id().clone())
            .collect();
        ids.dedup();
        Ok(ids)
    }

    async fn list_indexes(&self, _c: &CollectionId) -> Result<Vec<IndexInfo>, GatewayError> {
        self.record("list_indexes");
        Ok(Vec::new())
    }

    async fn write(&self, w: &Write, access: &WriteAccess) -> Result<UpdateTime, GatewayError> {
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
        let time = self.tick();
        let new_data: Option<Fields> = match w {
            Write::Set(s) => {
                match s.mode {
                    SetMode::Create if existing.is_some() => {
                        return Err(GatewayError::AlreadyExists(path.clone()));
                    }
                    SetMode::Create => {}
                    SetMode::Overwrite(g) | SetMode::Merge(g) => {
                        check_guard(path, existing.as_ref(), g)?;
                    }
                }
                Some(match s.mode {
                    SetMode::Merge(_) => existing
                        .map(|d| d.data)
                        .unwrap_or_default()
                        .into_iter()
                        .chain(s.data.clone())
                        .collect(),
                    SetMode::Create | SetMode::Overwrite(_) => s.data.clone(),
                })
            }
            Write::Update(u) => {
                let current = existing
                    .as_ref()
                    .ok_or_else(|| GatewayError::NotFound(path.clone()))?;
                check_guard(path, existing.as_ref(), u.guard)?;
                Some(u.ops.iter().try_fold(current.data.clone(), apply_op)?)
            }
            Write::Delete(d) => {
                if existing.is_none() {
                    return Err(GatewayError::NotFound(path.clone()));
                }
                check_guard(path, existing.as_ref(), d.guard)?;
                None
            }
        };
        match new_data {
            Some(data) => {
                self.docs.borrow_mut().insert(
                    path.clone(),
                    Document {
                        path: path.clone(),
                        update_time: time,
                        data,
                    },
                );
            }
            None => {
                self.docs.borrow_mut().remove(path);
            }
        }
        Ok(time)
    }
}
