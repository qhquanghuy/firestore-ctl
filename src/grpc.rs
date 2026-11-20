//! Every request names its database, so a named one is never replaced by `(default)`.

mod connect;
mod errors;
mod from_proto;
#[cfg(test)]
mod test_util;
mod to_proto;

use std::fmt;
use std::future::Future;

use gcloud_sdk::google::firestore::admin::v1::ListIndexesRequest;
use gcloud_sdk::google::firestore::admin::v1::firestore_admin_client::FirestoreAdminClient;
use gcloud_sdk::google::firestore::v1 as proto;
use gcloud_sdk::google::firestore::v1::batch_get_documents_response::Result as BatchResult;
use gcloud_sdk::google::firestore::v1::firestore_client::FirestoreClient;
use gcloud_sdk::tonic::{Request, Status, Streaming};
use gcloud_sdk::{GoogleApi, GoogleAuthMiddleware};

use crate::domain::{AggregateQuery, CollectionId, DbTarget, DocPath, Document, Query, Write};
use crate::policy::WriteAccess;
use crate::port::{AggregateResult, Firestore, GatewayError, IndexInfo, Written};

use self::errors::{map_status, write_error};

pub async fn connect(t: &DbTarget) -> Result<GrpcFirestore, GatewayError> {
    let endpoint = connect::endpoint(|name| std::env::var(name).ok());
    connect::open(t, &endpoint).await.map(|data| GrpcFirestore {
        target: t.clone(),
        data,
    })
}

pub struct GrpcFirestore {
    target: DbTarget,
    data: GoogleApi<FirestoreClient<GoogleAuthMiddleware>>,
}

impl fmt::Debug for GrpcFirestore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrpcFirestore")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

async fn drain<M>(mut stream: Streaming<M>) -> Result<Vec<M>, Status> {
    let mut messages = Vec::new();
    while let Some(message) = stream.message().await? {
        messages.push(message);
    }
    Ok(messages)
}

fn read_error(s: &Status) -> GatewayError {
    map_status(s, None)
}

async fn collect_pages<T, Fut>(fetch: impl Fn(String) -> Fut) -> Result<Vec<T>, GatewayError>
where
    Fut: Future<Output = Result<(Vec<T>, String), GatewayError>>,
{
    let mut items = Vec::new();
    let mut token = String::new();
    loop {
        let (page, next) = fetch(token).await?;
        items.extend(page);
        if next.is_empty() {
            return Ok(items);
        }
        token = next;
    }
}

impl GrpcFirestore {
    fn by_database<T>(&self, message: T) -> Request<T> {
        connect::routed(message, "database", &self.target.database_path())
    }

    fn admin(&self) -> FirestoreAdminClient<GoogleAuthMiddleware> {
        self.data.get_with(FirestoreAdminClient::new)
    }
}

impl Firestore for GrpcFirestore {
    async fn get(&self, p: &DocPath) -> Result<Option<Document>, GatewayError> {
        // BatchGet reports a missing document in-band, so a NOT_FOUND status here can
        // only be about the database. `GetDocument` could not tell the two apart.
        let request = proto::BatchGetDocumentsRequest {
            database: self.target.database_path(),
            documents: vec![to_proto::doc_name(p, &self.target)],
            ..Default::default()
        };
        let responses = self
            .data
            .get()
            .batch_get_documents(self.by_database(request))
            .await
            .map_err(|s| read_error(&s))?
            .into_inner();
        drain(responses)
            .await
            .map_err(|s| read_error(&s))?
            .into_iter()
            .find_map(|r| match r.result {
                Some(BatchResult::Found(d)) => Some(from_proto::document(d, &self.target)),
                Some(BatchResult::Missing(_)) | None => None,
            })
            .transpose()
    }

    async fn query(&self, q: &Query) -> Result<Vec<Document>, GatewayError> {
        let (parent, query) = to_proto::structured_query(q, &self.target);
        let request = proto::RunQueryRequest {
            parent: parent.clone(),
            query_type: Some(proto::run_query_request::QueryType::StructuredQuery(query)),
            ..Default::default()
        };
        let responses = self
            .data
            .get()
            .run_query(connect::routed(request, "parent", &parent))
            .await
            .map_err(|s| read_error(&s))?
            .into_inner();
        drain(responses)
            .await
            .map_err(|s| read_error(&s))?
            .into_iter()
            .filter_map(|r| r.document)
            .map(|d| from_proto::document(d, &self.target))
            .collect()
    }

    async fn aggregate(&self, q: &AggregateQuery) -> Result<AggregateResult, GatewayError> {
        let (parent, query) = to_proto::aggregation_query(q, &self.target);
        let request = proto::RunAggregationQueryRequest {
            parent: parent.clone(),
            query_type: Some(
                proto::run_aggregation_query_request::QueryType::StructuredAggregationQuery(query),
            ),
            ..Default::default()
        };
        let responses = self
            .data
            .get()
            .run_aggregation_query(connect::routed(request, "parent", &parent))
            .await
            .map_err(|s| read_error(&s))?
            .into_inner();
        let result = drain(responses)
            .await
            .map_err(|s| read_error(&s))?
            .into_iter()
            .find_map(|r| r.result)
            .ok_or_else(|| {
                GatewayError::Other("Firestore returned no aggregate result".to_owned())
            })?;
        q.aggregations
            .iter()
            .enumerate()
            .map(|(i, aggregation)| {
                let alias = to_proto::aggregation_alias(i);
                result
                    .aggregate_fields
                    .get(&alias)
                    .cloned()
                    .ok_or_else(|| {
                        GatewayError::Other(format!("Firestore returned no value for {alias}"))
                    })
                    .and_then(|v| from_proto::value(v, &self.target))
                    .map(|v| (aggregation.clone(), v))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(AggregateResult::new)
    }

    async fn list_collections(
        &self,
        parent: Option<&DocPath>,
    ) -> Result<Vec<CollectionId>, GatewayError> {
        let parent = &to_proto::parent_name(parent, &self.target);
        let ids = collect_pages(|page_token| async move {
            let request = proto::ListCollectionIdsRequest {
                parent: parent.clone(),
                page_token,
                ..Default::default()
            };
            let page = self
                .data
                .get()
                .list_collection_ids(connect::routed(request, "parent", parent))
                .await
                .map_err(|s| read_error(&s))?
                .into_inner();
            Ok((page.collection_ids, page.next_page_token))
        })
        .await?;
        ids.into_iter()
            .map(|id| {
                id.parse()
                    .map_err(|e| GatewayError::Other(format!("collection `{id}`: {e}")))
            })
            .collect()
    }

    async fn list_indexes(&self, c: &CollectionId) -> Result<Vec<IndexInfo>, GatewayError> {
        let parent = &format!("{}/collectionGroups/{c}", self.target.database_path());
        let indexes = collect_pages(|page_token| async move {
            let request = ListIndexesRequest {
                parent: parent.clone(),
                page_token,
                ..Default::default()
            };
            let page = self
                .admin()
                .list_indexes(connect::routed(request, "parent", parent))
                .await
                .map_err(|s| read_error(&s))?
                .into_inner();
            Ok((page.indexes, page.next_page_token))
        })
        .await?;
        Ok(indexes
            .iter()
            .filter_map(|i| from_proto::index(c, i))
            .collect())
    }

    async fn write(&self, w: &Write, access: &WriteAccess) -> Result<Written, GatewayError> {
        if access.target() != &self.target {
            return Err(GatewayError::Other(format!(
                "refusing to write to {}/{}: this connection is to {}/{}",
                access.target().project,
                access.target().database,
                self.target.project,
                self.target.database,
            )));
        }
        let request = proto::CommitRequest {
            database: self.target.database_path(),
            writes: vec![to_proto::write(w, &self.target)?],
            ..Default::default()
        };
        let response = self
            .data
            .get()
            .commit(self.by_database(request))
            .await
            .map_err(|s| write_error(&s, w))?
            .into_inner();
        match w {
            Write::Delete(_) => Ok(Written::Deleted),
            Write::Set(_) | Write::Update(_) => response
                .write_results
                .first()
                .and_then(|r| r.update_time.as_ref())
                .or(response.commit_time.as_ref())
                .ok_or_else(|| GatewayError::Other("Firestore returned no update time".to_owned()))
                .and_then(from_proto::update_time)
                .map(Written::Updated),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DeleteDoc, Guard};
    use crate::grpc::test_util::{doc, target};
    use crate::policy::WritePolicy;

    #[tokio::test]
    async fn collect_pages_follows_tokens_until_empty_and_stops_on_error() {
        let seen = std::cell::RefCell::new(Vec::new());
        let pages = collect_pages(|token: String| {
            seen.borrow_mut().push(token.clone());
            async move {
                match token.as_str() {
                    "" => Ok((vec![1, 2], "a".to_owned())),
                    "a" => Ok((vec![3], "b".to_owned())),
                    _ => Ok((vec![4], String::new())),
                }
            }
        })
        .await;
        assert_eq!(pages, Ok(vec![1, 2, 3, 4]));
        assert_eq!(*seen.borrow(), ["", "a", "b"]);

        let failed: Result<Vec<u8>, _> =
            collect_pages(|_| async { Err(GatewayError::Other("boom".to_owned())) }).await;
        assert_eq!(failed, Err(GatewayError::Other("boom".to_owned())));
    }

    #[tokio::test]
    async fn write_refuses_a_grant_for_another_database() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint =
            connect::Endpoint::Emulator(format!("http://{}", listener.local_addr().unwrap()));
        let connected = target("p", "d");
        let data = connect::open(&connected, &endpoint).await.unwrap();
        let fs = GrpcFirestore {
            target: connected,
            data,
        };

        let elsewhere = target("p", "other");
        let access = WritePolicy::from_env_value(Some("p/other"))
            .unwrap()
            .authorize(&elsewhere)
            .unwrap();
        let delete = Write::Delete(DeleteDoc {
            path: doc("users/u1"),
            guard: Guard::None,
        });
        assert!(matches!(
            fs.write(&delete, &access).await,
            Err(GatewayError::Other(m)) if m.contains("p/other") && m.contains("p/d")
        ));
    }
}
