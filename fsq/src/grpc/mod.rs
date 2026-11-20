//! The gRPC adapter for the Firestore port. For now only a connection stub: every
//! operation fails with "not implemented" until the real adapter replaces it.

use crate::domain::{AggregateQuery, CollectionId, DbTarget, DocPath, Document, Query, Write};
use crate::policy::WriteAccess;
use crate::port::{AggregateResult, Firestore, GatewayError, IndexInfo, Written};

/// Connects to the database `t`.
///
/// # Errors
/// [`GatewayError`] if the connection or authentication fails.
// Async because the real connection will await; the stub has nothing to await yet.
#[allow(clippy::unused_async)]
pub async fn connect(_t: &DbTarget) -> Result<impl Firestore, GatewayError> {
    Ok(NotImplemented)
}

/// A stand-in whose every operation fails.
#[derive(Debug)]
struct NotImplemented;

fn unavailable<T>() -> Result<T, GatewayError> {
    Err(GatewayError::Other("not implemented".to_owned()))
}

// Stub: nothing to await until the real adapter lands.
#[allow(clippy::unused_async_trait_impl)]
impl Firestore for NotImplemented {
    async fn get(&self, _p: &DocPath) -> Result<Option<Document>, GatewayError> {
        unavailable()
    }

    async fn query(&self, _q: &Query) -> Result<Vec<Document>, GatewayError> {
        unavailable()
    }

    async fn aggregate(&self, _q: &AggregateQuery) -> Result<AggregateResult, GatewayError> {
        unavailable()
    }

    async fn list_collections(
        &self,
        _parent: Option<&DocPath>,
    ) -> Result<Vec<CollectionId>, GatewayError> {
        unavailable()
    }

    async fn list_indexes(&self, _c: &CollectionId) -> Result<Vec<IndexInfo>, GatewayError> {
        unavailable()
    }

    async fn write(&self, _w: &Write, _access: &WriteAccess) -> Result<Written, GatewayError> {
        unavailable()
    }
}
