use gcloud_sdk::tonic::{Code, Status};

use super::connect::CALL_TIMEOUT;
use crate::domain::{DocPath, Write};
use crate::port::GatewayError;

/// A failed precondition is classified by [`write_error`] (it depends on the write's guard); elsewhere it
/// is a missing index only if its message says so.
#[must_use]
pub(super) fn map_status(s: &Status, path: Option<&DocPath>) -> GatewayError {
    let message = s.message();
    match (s.code(), path) {
        (Code::NotFound, Some(p)) if !names_missing_database(message) => {
            GatewayError::NotFound(p.clone())
        }
        (Code::NotFound, _) => GatewayError::ResourceNotFound(message.to_owned()),
        (Code::AlreadyExists, Some(p)) => GatewayError::AlreadyExists(p.clone()),
        (Code::FailedPrecondition, _) if message.to_lowercase().contains("index") => {
            GatewayError::MissingIndex {
                console_url: first_https_url(message),
            }
        }
        (Code::Unauthenticated, _) => GatewayError::Auth(format!("unauthenticated: {message}")),
        (Code::PermissionDenied, _) => GatewayError::PermissionDenied(message.to_owned()),
        (Code::DeadlineExceeded, _) => GatewayError::Other(format!(
            "Firestore call timed out after {}s ({message})",
            CALL_TIMEOUT.as_secs()
        )),
        // The auth middleware fails a call before it is sent when it cannot get a token;
        // tonic reports that as `Unknown` with the middleware's message.
        (Code::Unknown, _) if message.contains("credentials error") => {
            GatewayError::Auth(message.to_owned())
        }
        (code, _) => GatewayError::Other(format!("Firestore returned {code:?}: {message}")),
    }
}

/// A guarded write needs the document to exist with the guarded update time, and Firestore reports a
/// missing or changed document as `FAILED_PRECONDITION` or `NOT_FOUND`: both are
/// [`GatewayError::PreconditionFailed`]. Without a guard, `FAILED_PRECONDITION` is not the document's
/// doing and `NOT_FOUND` means the document is missing.
#[must_use]
pub(super) fn write_error(s: &Status, w: &Write) -> GatewayError {
    let precondition = || GatewayError::PreconditionFailed(w.path().clone());
    match (s.code(), w.guard()) {
        (Code::FailedPrecondition, Some(_)) => precondition(),
        (_, guard) => match (map_status(s, Some(w.path())), guard) {
            (GatewayError::NotFound(_), Some(_)) => precondition(),
            (e, _) => e,
        },
    }
}

/// Document messages quote the document's resource name, which contains `/documents/`; database
/// and project ones do not.
fn names_missing_database(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("does not exist") && !lower.contains("/documents/")
}

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
        let s = Status::unauthenticated("bad token");
        assert!(matches!(map_status(&s, None), GatewayError::Auth(_)));
        assert!(matches!(
            map_status(&s, Some(&doc("users/u1"))),
            GatewayError::Auth(_)
        ));
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
    fn already_exists_needs_a_document() {
        let p = doc("users/u1");
        assert_eq!(
            map_status(&Status::already_exists("Document already exists"), Some(&p)),
            GatewayError::AlreadyExists(p)
        );
        assert!(matches!(
            map_status(&Status::already_exists("x"), None),
            GatewayError::Other(_)
        ));
    }

    #[test]
    fn failed_precondition_is_other_unless_it_names_an_index() {
        for path in [None, Some(doc("users/u1"))] {
            assert!(matches!(
                map_status(
                    &Status::failed_precondition("something else"),
                    path.as_ref()
                ),
                GatewayError::Other(_)
            ));
            assert!(matches!(
                map_status(
                    &Status::failed_precondition("needs an index"),
                    path.as_ref()
                ),
                GatewayError::MissingIndex { .. }
            ));
        }
    }

    #[test]
    fn deadline_exceeded_says_the_call_timed_out() {
        let s = Status::deadline_exceeded("total request timeout of 60s elapsed");
        assert!(matches!(
            map_status(&s, None),
            GatewayError::Other(m) if m.contains("timed out after 60s")
        ));
    }

    #[test]
    fn permission_denied_is_distinct_from_authentication() {
        assert!(matches!(
            map_status(&Status::permission_denied("no access"), None),
            GatewayError::PermissionDenied(m) if m == "no access"
        ));
        assert!(matches!(
            map_status(&Status::unauthenticated("bad token"), None),
            GatewayError::Auth(_)
        ));
    }

    mod writes {
        use super::*;
        use crate::domain::{DeleteDoc, Fields, Guard, SetDoc, SetMode};

        fn guard() -> Guard {
            Guard::UnchangedSince("2026-01-05T10:00:00Z".parse().unwrap())
        }

        fn delete(guard: Guard) -> Write {
            Write::Delete(DeleteDoc {
                path: doc("users/u1"),
                guard,
            })
        }

        fn create() -> Write {
            Write::Set(SetDoc {
                path: doc("users/u1"),
                data: Fields::new(),
                mode: SetMode::Create,
            })
        }

        fn p() -> DocPath {
            doc("users/u1")
        }

        #[test]
        fn guarded_not_found_is_a_failed_precondition() {
            let s = Status::not_found(
                "No document to update: projects/p/databases/d/documents/users/u1",
            );
            assert_eq!(
                write_error(&s, &delete(guard())),
                GatewayError::PreconditionFailed(p())
            );
        }

        #[test]
        fn unguarded_not_found_is_not_found() {
            let s = Status::not_found(
                "No document to delete: projects/p/databases/d/documents/users/u1",
            );
            assert_eq!(
                write_error(&s, &delete(Guard::None)),
                GatewayError::NotFound(p())
            );
        }

        #[test]
        fn guarded_failed_precondition_is_a_failed_precondition_even_if_it_says_index() {
            for message in [
                "the stored version does not match",
                "stored version of the index differs",
            ] {
                let s = Status::failed_precondition(message);
                assert_eq!(
                    write_error(&s, &delete(guard())),
                    GatewayError::PreconditionFailed(p())
                );
            }
        }

        #[test]
        fn unguarded_failed_precondition_is_not_a_failed_precondition() {
            let s = Status::failed_precondition("transaction conflict");
            assert!(matches!(
                write_error(&s, &delete(Guard::None)),
                GatewayError::Other(_)
            ));
            assert!(matches!(write_error(&s, &create()), GatewayError::Other(_)));
        }

        #[test]
        fn create_on_an_existing_document_is_already_exists() {
            let s = Status::already_exists("Document already exists");
            assert_eq!(write_error(&s, &create()), GatewayError::AlreadyExists(p()));
        }

        #[test]
        fn guarded_write_to_a_missing_database_stays_resource_not_found() {
            let s = Status::not_found("The database projects/p/databases/x does not exist.");
            assert!(matches!(
                write_error(&s, &delete(guard())),
                GatewayError::ResourceNotFound(_)
            ));
        }
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
