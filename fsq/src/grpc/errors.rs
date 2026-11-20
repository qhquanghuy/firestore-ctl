//! gRPC statuses to [`GatewayError`]. Pure.

use gcloud_sdk::tonic::{Code, Status};

use crate::domain::DocPath;
use crate::port::GatewayError;

/// Classifies a failed RPC.
///
/// `path` is the document the call was about (a read or write of one document), or
/// `None` for queries and listings. It decides what a status means: a missing or
/// precondition-failing *document* needs a path to name; a missing *index* can only
/// come from a query, so a write whose message mentions an index is not one.
#[must_use]
pub fn map_status(s: &Status, path: Option<&DocPath>) -> GatewayError {
    let message = s.message();
    match (s.code(), path) {
        (Code::NotFound, Some(p)) if !names_missing_database(message) => {
            GatewayError::NotFound(p.clone())
        }
        (Code::NotFound, _) => GatewayError::ResourceNotFound(message.to_owned()),
        (Code::AlreadyExists, Some(p)) => GatewayError::AlreadyExists(p.clone()),
        (Code::FailedPrecondition, None) if message.to_lowercase().contains("index") => {
            GatewayError::MissingIndex {
                console_url: first_https_url(message),
            }
        }
        (Code::FailedPrecondition, Some(p)) => GatewayError::PreconditionFailed(p.clone()),
        (Code::Unauthenticated, _) => GatewayError::Auth(format!("unauthenticated: {message}")),
        (Code::PermissionDenied, _) => GatewayError::Auth(format!("permission denied: {message}")),
        // The auth middleware fails a call before it is sent when it cannot get a token;
        // tonic reports that as `Unknown` with the middleware's message.
        (Code::Unknown, _) if message.contains("credentials error") => {
            GatewayError::Auth(message.to_owned())
        }
        (code, _) => GatewayError::Other(format!("Firestore returned {code:?}: {message}")),
    }
}

/// Whether a `NOT_FOUND` message is about the database or project rather than a document.
/// Document messages quote the document's resource name, which contains `/documents/`.
fn names_missing_database(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("does not exist") && !lower.contains("/documents/")
}

/// The first `https://` URL in `message`, without trailing sentence punctuation.
fn first_https_url(message: &str) -> Option<String> {
    message
        .split_whitespace()
        .find(|word| word.starts_with("https://"))
        .map(|url| {
            url.trim_end_matches(['.', ',', ';', ')', '"', '\'', '>'])
                .to_owned()
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::grpc::test_util::doc;

    #[test]
    fn missing_index_status_extracts_url() {
        let s = Status::failed_precondition(
            "The query requires an index. You can create it here: https://console.firebase.google.com/x?create_composite=abc",
        );
        assert!(matches!(
            map_status(&s, None),
            GatewayError::MissingIndex { console_url: Some(u) }
                if u == "https://console.firebase.google.com/x?create_composite=abc"
        ));
    }

    #[test]
    fn index_url_loses_trailing_punctuation() {
        let s =
            Status::failed_precondition("requires an index (create: https://example.com/i?a=b).");
        assert!(matches!(
            map_status(&s, None),
            GatewayError::MissingIndex { console_url: Some(u) } if u == "https://example.com/i?a=b"
        ));
    }

    #[test]
    fn missing_index_without_url_has_none() {
        let s = Status::failed_precondition("The query requires an INDEX.");
        assert_eq!(
            map_status(&s, None),
            GatewayError::MissingIndex { console_url: None }
        );
    }

    #[test]
    fn auth_errors_map_to_auth_kind() {
        for s in [
            Status::unauthenticated("bad token"),
            Status::permission_denied("no access"),
        ] {
            assert!(
                matches!(map_status(&s, None), GatewayError::Auth(_)),
                "{s:?}"
            );
            assert!(matches!(
                map_status(&s, Some(&doc("users/u1"))),
                GatewayError::Auth(_)
            ));
        }
    }

    #[test]
    fn credential_failures_surfaced_by_the_transport_are_auth() {
        let s = Status::unknown("Service was not ready: credentials error: token expired");
        assert!(matches!(map_status(&s, None), GatewayError::Auth(m) if m.contains("expired")));
    }

    #[test]
    fn not_found_names_the_document_when_there_is_one() {
        let p = doc("users/u1");
        let s =
            Status::not_found("No document to update: projects/p/databases/d/documents/users/u1");
        assert_eq!(map_status(&s, Some(&p)), GatewayError::NotFound(p));
        assert!(matches!(
            map_status(&s, None),
            GatewayError::ResourceNotFound(_)
        ));
    }

    #[test]
    fn missing_database_is_a_resource_not_found_even_for_a_document_call() {
        let s = Status::not_found(
            "The database projects/p/databases/nope does not exist for project p.",
        );
        assert!(matches!(
            map_status(&s, Some(&doc("users/u1"))),
            GatewayError::ResourceNotFound(_)
        ));
    }

    #[test]
    fn already_exists_and_precondition_need_a_document() {
        let p = doc("users/u1");
        assert_eq!(
            map_status(&Status::already_exists("Document already exists"), Some(&p)),
            GatewayError::AlreadyExists(p.clone())
        );
        assert_eq!(
            map_status(
                &Status::failed_precondition("the stored version does not match"),
                Some(&p)
            ),
            GatewayError::PreconditionFailed(p.clone())
        );
        assert!(matches!(
            map_status(&Status::failed_precondition("something else"), None),
            GatewayError::Other(_)
        ));
    }

    #[test]
    fn write_precondition_mentioning_index_is_not_a_missing_index() {
        let s = Status::failed_precondition("stored version of the index document differs");
        assert!(matches!(
            map_status(&s, Some(&doc("users/u1"))),
            GatewayError::PreconditionFailed(_)
        ));
    }

    #[test]
    fn other_statuses_keep_code_and_message() {
        let s = Status::invalid_argument("bad field");
        assert!(matches!(
            map_status(&s, None),
            GatewayError::Other(m) if m.contains("InvalidArgument") && m.contains("bad field")
        ));
    }
}
