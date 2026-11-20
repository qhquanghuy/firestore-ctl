//! The central flow: [`Request`] → write authorization → Firestore port → [`Output`].

use thiserror::Error;

use crate::domain::{
    AggregateQuery, Aggregation, CollectionId, DbTarget, DocPath, Document, FieldPath, IndexAdvice,
    Query, Write, required_index,
};
use crate::policy::{PolicyError, WriteAccess, WriteDenied, WritePolicy};
use crate::port::{AggregateResult, Firestore, GatewayError, IndexInfo, Written};

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Get(DocPath),
    Query(Query),
    Aggregate(AggregateQuery),
    Collections(Option<DocPath>),
    Indexes(CollectionId),
    Write(Write),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Document(Document),
    Documents {
        docs: Vec<Document>,
        /// The selected field paths; the documents hold only these fields.
        select: Option<Vec<FieldPath>>,
    },
    Aggregate(AggregateResult),
    Collections(Vec<CollectionId>),
    Indexes(Vec<IndexInfo>),
    Written {
        path: DocPath,
        result: Written,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AppError {
    #[error(transparent)]
    Gateway(GatewayError),
    #[error("the query requires an index that does not exist")]
    MissingIndex {
        advice: Option<IndexAdvice>,
        console_url: Option<String>,
    },
    #[error(transparent)]
    WritesDisabled(WriteDenied),
    #[error(transparent)]
    Policy(#[from] PolicyError),
    #[error("document not found: {0}")]
    NotFound(DocPath),
    #[error("{0}")]
    Input(String),
}

#[derive(Debug, Clone)]
pub struct Context {
    pub target: DbTarget,
    pub policy: WritePolicy,
}

/// A [`Request`] cleared to run, obtainable only from [`authorize`]. A write carries its
/// [`WriteAccess`], so [`run`] cannot be handed an unauthorized write.
#[derive(Debug)]
pub struct Authorized(Cleared);

#[derive(Debug)]
enum Cleared {
    Get(DocPath),
    Query(Query),
    Aggregate(AggregateQuery),
    Collections(Option<DocPath>),
    Indexes(CollectionId),
    Write(Write, WriteAccess),
}

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

/// Executes an authorized request; returns a value, never prints.
///
/// # Errors
/// [`AppError::NotFound`] for `get` of a missing document, [`AppError::MissingIndex`] for a
/// query lacking an index, otherwise [`AppError::Gateway`].
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

fn index_error(e: GatewayError, advice: impl FnOnce() -> Option<IndexAdvice>) -> AppError {
    match e {
        GatewayError::MissingIndex { console_url } => AppError::MissingIndex {
            advice: advice(),
            console_url,
        },
        other => AppError::Gateway(other),
    }
}

/// Only a plain `count` gets advice: `sum`/`avg` need an index that also covers the
/// aggregated field, which advice for the filter and order fields alone would omit, so those
/// get the console link only.
fn aggregate_advice(a: &AggregateQuery) -> Option<IndexAdvice> {
    a.aggregations
        .iter()
        .all(|aggregation| matches!(aggregation, Aggregation::Count))
        .then(|| required_index(&a.query))
        .flatten()
}
