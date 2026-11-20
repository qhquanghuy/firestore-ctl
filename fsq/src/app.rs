//! The central flow: [`Request`] → write authorization → Firestore port → [`Output`].

use thiserror::Error;

use crate::domain::{
    AggregateQuery, CollectionId, DbTarget, DocPath, Document, IndexAdvice, Query, UpdateTime,
    Write, required_index,
};
use crate::policy::{WriteAccess, WriteDenied, WritePolicy};
use crate::port::{AggregateResult, Firestore, GatewayError, IndexInfo};

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
    /// Query results.
    Documents(Vec<Document>),
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
        /// Its update time after the write.
        update_time: UpdateTime,
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

/// Authorizes writes *before* touching the gateway; returns a value, never prints.
///
/// # Errors
/// [`AppError::WritesDisabled`] for a write to a read-only database; otherwise the
/// failure of the Firestore call ([`AppError::NotFound`] for `get` of a missing
/// document, [`AppError::MissingIndex`] for a query lacking an index).
pub async fn run<F: Firestore>(ctx: &Context, fs: &F, req: Request) -> Result<Output, AppError> {
    match req {
        // Authorized here as well as in `preflight` (which main calls before
        // connecting): no write path may reach the gateway without a grant.
        Request::Write(w) => {
            let access = authorize_write(ctx)?;
            fs.write(&w, &access)
                .await
                .map(|update_time| Output::Written {
                    path: w.path().clone(),
                    update_time,
                })
                .map_err(AppError::Gateway)
        }
        Request::Get(p) => fs
            .get(&p)
            .await
            .map_err(AppError::Gateway)?
            .map(Output::Document)
            .ok_or(AppError::NotFound(p)),
        Request::Query(q) => fs
            .query(&q)
            .await
            .map(Output::Documents)
            .map_err(|e| index_error(e, &q)),
        Request::Aggregate(a) => fs
            .aggregate(&a)
            .await
            .map(Output::Aggregate)
            .map_err(|e| index_error(e, &a.query)),
        Request::Collections(parent) => fs
            .list_collections(parent.as_ref())
            .await
            .map(Output::Collections)
            .map_err(AppError::Gateway),
        Request::Indexes(c) => fs
            .list_indexes(&c)
            .await
            .map(Output::Indexes)
            .map_err(AppError::Gateway),
    }
}

/// Writes are authorized before any connection is opened; main.rs calls this first.
/// Returns the grant for a write and `None` for reads.
///
/// # Errors
/// [`AppError::WritesDisabled`] if `req` is a write and the target is not writable.
pub fn preflight(ctx: &Context, req: &Request) -> Result<Option<WriteAccess>, AppError> {
    match req {
        Request::Write(_) => authorize_write(ctx).map(Some),
        _ => Ok(None),
    }
}

fn authorize_write(ctx: &Context) -> Result<WriteAccess, AppError> {
    ctx.policy
        .authorize(&ctx.target)
        .map_err(AppError::WritesDisabled)
}

/// Turns a gateway's missing-index failure into advice for the query `q`.
fn index_error(e: GatewayError, q: &Query) -> AppError {
    match e {
        GatewayError::MissingIndex { console_url } => AppError::MissingIndex {
            advice: required_index(q),
            console_url,
        },
        other => AppError::Gateway(other),
    }
}
