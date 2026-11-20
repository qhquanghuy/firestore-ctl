use thiserror::Error;

use crate::domain::{
    AggregateQuery, Aggregation, CollectionId, DocPath, Document, IndexSpec, Query, UpdateTime,
    Value, Write,
};
use crate::policy::WriteAccess;

#[derive(Debug, Clone, PartialEq)]
pub struct AggregateResult(Vec<(Aggregation, Value)>);

impl AggregateResult {
    #[must_use]
    pub fn new(entries: Vec<(Aggregation, Value)>) -> Self {
        Self(entries)
    }

    pub fn entries(&self) -> impl Iterator<Item = &(Aggregation, Value)> {
        self.0.iter()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    Updated(UpdateTime),
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexState {
    Ready,
    Creating,
    NeedsRepair,
    Unknown,
}

impl IndexState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Creating => "creating",
            Self::NeedsRepair => "needs-repair",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexInfo {
    pub spec: IndexSpec,
    pub state: IndexState,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GatewayError {
    #[error("document not found: {0}")]
    NotFound(DocPath),
    #[error("document already exists: {0}")]
    AlreadyExists(DocPath),
    /// Firestore reports a missing and a changed document the same way.
    #[error("precondition failed: {0} is missing or changed since the given time")]
    PreconditionFailed(DocPath),
    #[error("the query requires an index that does not exist")]
    MissingIndex { console_url: Option<String> },
    #[error("not found: {0}")]
    ResourceNotFound(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("{0}")]
    Other(String),
}

// The futures are awaited in place and never spawned, so the missing `Send` bounds are irrelevant.
#[allow(async_fn_in_trait)]
pub trait Firestore {
    async fn get(&self, p: &DocPath) -> Result<Option<Document>, GatewayError>;

    async fn query(&self, q: &Query) -> Result<Vec<Document>, GatewayError>;

    async fn aggregate(&self, q: &AggregateQuery) -> Result<AggregateResult, GatewayError>;

    async fn list_collections(
        &self,
        parent: Option<&DocPath>,
    ) -> Result<Vec<CollectionId>, GatewayError>;

    async fn list_indexes(&self, c: &CollectionId) -> Result<Vec<IndexInfo>, GatewayError>;

    /// An implementation connected to a different database must refuse the write. Firestore cannot
    /// combine `exists` with `update_time`, so a guard replaces the existence check. Every
    /// implementation enforces:
    /// - `Set` with `SetMode::Create`: the document must not exist, else
    ///   [`GatewayError::AlreadyExists`].
    /// - `Set` with `Overwrite` or `Merge` and `Guard::None`: no precondition (upsert).
    /// - `Update` or `Delete` with `Guard::None`: the document must exist, else
    ///   [`GatewayError::NotFound`].
    /// - Any write with `Guard::UnchangedSince(t)`: the document must exist with update
    ///   time `t`, else [`GatewayError::PreconditionFailed`] (missing or changed).
    async fn write(&self, w: &Write, access: &WriteAccess) -> Result<Written, GatewayError>;
}
