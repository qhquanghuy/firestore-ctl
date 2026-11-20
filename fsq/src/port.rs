//! The Firestore port: the only trait in the crate. The real gRPC adapter and the
//! test fake both implement it; the rest of the program is generic over it.

use thiserror::Error;

use crate::domain::{
    AggregateQuery, Aggregation, CollectionId, DocPath, Document, IndexSpec, Query, UpdateTime,
    Value, Write,
};
use crate::policy::WriteAccess;

/// Results of an aggregate query, in the order the aggregations were requested.
#[derive(Debug, Clone, PartialEq)]
pub struct AggregateResult(Vec<(Aggregation, Value)>);

impl AggregateResult {
    /// Pairs each aggregation with its computed value.
    #[must_use]
    pub fn new(entries: Vec<(Aggregation, Value)>) -> Self {
        Self(entries)
    }

    /// The aggregations and their values, in request order.
    pub fn entries(&self) -> impl Iterator<Item = &(Aggregation, Value)> {
        self.0.iter()
    }
}

/// How far along a composite index is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexState {
    /// Serving queries.
    Ready,
    /// Still building.
    Creating,
    /// Failed and needs to be recreated.
    NeedsRepair,
    /// The server reported a state this program does not know.
    Unknown,
}

/// An existing index and its state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexInfo {
    /// What the index covers.
    pub spec: IndexSpec,
    /// Whether it is usable yet.
    pub state: IndexState,
}

/// A failure reported by (or while reaching) Firestore.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GatewayError {
    /// The document does not exist.
    #[error("document not found: {0}")]
    NotFound(DocPath),
    /// The document already exists.
    #[error("document already exists: {0}")]
    AlreadyExists(DocPath),
    /// The document changed since the guarded `_updateTime`, or a guard needs it to exist.
    #[error("precondition failed: {0} changed since the given time")]
    PreconditionFailed(DocPath),
    /// The query needs an index that does not exist.
    #[error("the query requires an index that does not exist")]
    MissingIndex {
        /// Console link that creates the index, when the server provided one.
        console_url: Option<String>,
    },
    /// Missing, expired or rejected credentials.
    #[error("authentication failed: {0}")]
    Auth(String),
    /// Any other failure (network, quota, malformed request, ...).
    #[error("{0}")]
    Other(String),
}

/// Everything the program asks of Firestore. Implemented by the gRPC adapter and by
/// the test fake; used only through generics (no `dyn`).
// The program runs on a single-threaded runtime and never spawns these futures, so
// the missing `Send` bounds the lint warns about are irrelevant.
#[allow(async_fn_in_trait)]
pub trait Firestore {
    /// The document at `p`, or `None` if it does not exist.
    async fn get(&self, p: &DocPath) -> Result<Option<Document>, GatewayError>;

    /// The documents matching `q`.
    async fn query(&self, q: &Query) -> Result<Vec<Document>, GatewayError>;

    /// The aggregates of `q`.
    async fn aggregate(&self, q: &AggregateQuery) -> Result<AggregateResult, GatewayError>;

    /// The collections under `parent`, or the root collections when `None`.
    async fn list_collections(
        &self,
        parent: Option<&DocPath>,
    ) -> Result<Vec<CollectionId>, GatewayError>;

    /// The composite indexes of collection group `c`.
    async fn list_indexes(&self, c: &CollectionId) -> Result<Vec<IndexInfo>, GatewayError>;

    /// Applies `w` and returns the document's new update time.
    ///
    /// The database written is `access.target()`: the grant names the database, so
    /// there is no separate target parameter that could disagree with it. An
    /// implementation connected to a different database must refuse the write.
    async fn write(&self, w: &Write, access: &WriteAccess) -> Result<UpdateTime, GatewayError>;
}
