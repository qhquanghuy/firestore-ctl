//! The central flow: [`Request`] → write authorization → Firestore port → [`Output`].

use thiserror::Error;

use crate::domain::{
    AggregateQuery, Aggregation, CollectionId, DbTarget, DocPath, Document, FieldPath, IndexAdvice,
    Query, Write, required_index,
};
use crate::policy::{PolicyError, WriteAccess, WriteDenied, WritePolicy};
use crate::port::{AggregateResult, Firestore, GatewayError, IndexInfo, Written};

/// What the user asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Fetch one document.
    Get(DocPath),
    /// Run a document query.
    Query(Query),
    /// Run an aggregate query.
    Aggregate(AggregateQuery),
    /// List collections under a document, or the root collections.
    Collections(Option<DocPath>),
    /// List the composite indexes of a collection group.
    Indexes(CollectionId),
    /// Change data (needs write authorization).
    Write(Write),
}

/// What a successful [`Request`] produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    /// A single document.
    Document(Document),
    /// Query results, with the projection the query applied (`--select`), if any.
    Documents {
        /// The matching documents.
        docs: Vec<Document>,
        /// The selected field paths; the documents hold only these fields.
        select: Option<Vec<FieldPath>>,
    },
    /// Aggregate values.
    Aggregate(AggregateResult),
    /// Collection names.
    Collections(Vec<CollectionId>),
    /// Existing indexes.
    Indexes(Vec<IndexInfo>),
    /// A write succeeded.
    Written {
        /// The document written.
        path: DocPath,
        /// What the write left behind.
        result: Written,
    },
}

/// Why a [`Request`] failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AppError {
    /// Firestore (or the connection to it) failed.
    #[error(transparent)]
    Gateway(GatewayError),
    /// The query needs an index; `advice` says which, when it can be worked out.
    #[error("the query requires an index that does not exist")]
    MissingIndex {
        /// The index to create.
        advice: Option<IndexAdvice>,
        /// Console link that creates the index, when the server provided one.
        console_url: Option<String>,
    },
    /// The target database is not writable.
    #[error(transparent)]
    WritesDisabled(WriteDenied),
    /// The write policy (`FSQ_WRITABLE`) is malformed, so no write can be authorized.
    #[error(transparent)]
    Policy(#[from] PolicyError),
    /// A requested document does not exist.
    #[error("document not found: {0}")]
    NotFound(DocPath),
    /// The input was rejected before reaching Firestore.
    #[error("{0}")]
    Input(String),
}

/// The database in use and who may write to it.
#[derive(Debug, Clone)]
pub struct Context {
    /// The database every request goes to.
    pub target: DbTarget,
    /// Which databases accept writes.
    pub policy: WritePolicy,
}

/// A [`Request`] cleared to run, obtained only from [`authorize`]. A write carries the
/// [`WriteAccess`] granted for it, so [`run`] cannot be handed an unauthorized write
/// and holds no policy of its own.
#[derive(Debug)]
pub struct Authorized(Cleared);

/// [`Request`] with the grant attached to its write: no write without access.
#[derive(Debug)]
enum Cleared {
    Get(DocPath),
    Query(Query),
    Aggregate(AggregateQuery),
    Collections(Option<DocPath>),
    Indexes(CollectionId),
    Write(Write, WriteAccess),
}

/// Checks `req` against the write policy; main.rs calls this before connecting.
/// Writes get a grant for the context's database, reads need none.
///
/// # Errors
/// [`AppError::WritesDisabled`] if `req` is a write and the target is not writable.
pub fn authorize(ctx: &Context, req: Request) -> Result<Authorized, AppError> {
    match req {
        Request::Write(w) => ctx
            .policy
            .authorize(&ctx.target)
            .map(|access| Cleared::Write(w, access))
            .map_err(AppError::WritesDisabled),
        Request::Get(p) => Ok(Cleared::Get(p)),
        Request::Query(q) => Ok(Cleared::Query(q)),
        Request::Aggregate(a) => Ok(Cleared::Aggregate(a)),
        Request::Collections(parent) => Ok(Cleared::Collections(parent)),
        Request::Indexes(c) => Ok(Cleared::Indexes(c)),
    }
    .map(Authorized)
}

/// Executes an authorized request against Firestore; returns a value, never prints.
///
/// # Errors
/// The failure of the Firestore call: [`AppError::NotFound`] for `get` of a missing
/// document, [`AppError::MissingIndex`] for a query lacking an index, otherwise
/// [`AppError::Gateway`].
pub async fn run<F: Firestore>(fs: &F, req: Authorized) -> Result<Output, AppError> {
    match req.0 {
        Cleared::Write(w, access) => fs
            .write(&w, &access)
            .await
            .map(|result| Output::Written {
                path: w.path().clone(),
                result,
            })
            .map_err(AppError::Gateway),
        Cleared::Get(p) => fs
            .get(&p)
            .await
            .map_err(AppError::Gateway)?
            .map(Output::Document)
            .ok_or(AppError::NotFound(p)),
        Cleared::Query(q) => fs
            .query(&q)
            .await
            .map(|docs| Output::Documents {
                docs,
                select: q.select.clone(),
            })
            .map_err(|e| index_error(e, || required_index(&q))),
        Cleared::Aggregate(a) => fs
            .aggregate(&a)
            .await
            .map(Output::Aggregate)
            .map_err(|e| index_error(e, || aggregate_advice(&a))),
        Cleared::Collections(parent) => fs
            .list_collections(parent.as_ref())
            .await
            .map(Output::Collections)
            .map_err(AppError::Gateway),
        Cleared::Indexes(c) => fs
            .list_indexes(&c)
            .await
            .map(Output::Indexes)
            .map_err(AppError::Gateway),
    }
}

/// Turns a gateway's missing-index failure into an [`AppError::MissingIndex`] carrying
/// the `advice` computed on demand.
fn index_error(e: GatewayError, advice: impl FnOnce() -> Option<IndexAdvice>) -> AppError {
    match e {
        GatewayError::MissingIndex { console_url } => AppError::MissingIndex {
            advice: advice(),
            console_url,
        },
        other => AppError::Gateway(other),
    }
}

/// The index advice for an aggregate query: only for a plain `count`. A `sum` or `avg`
/// is served by an index that also covers the aggregated field, which the advice for the
/// filter and order fields alone would leave out, so those get the console link only.
fn aggregate_advice(a: &AggregateQuery) -> Option<IndexAdvice> {
    a.aggregations
        .iter()
        .all(|aggregation| matches!(aggregation, Aggregation::Count))
        .then(|| required_index(&a.query))
        .flatten()
}
