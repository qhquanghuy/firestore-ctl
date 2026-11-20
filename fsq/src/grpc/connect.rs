//! Connection setup: endpoint, credentials and routing headers.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use gcloud_sdk::error::ErrorKind;
use gcloud_sdk::google::firestore::v1::firestore_client::FirestoreClient;
use gcloud_sdk::google_cloud_auth::credentials::{
    CacheableResource, Credentials, CredentialsProvider, EntityTag,
};
use gcloud_sdk::google_cloud_auth::errors::CredentialsError;
use gcloud_sdk::tonic::codegen::http::HeaderValue;
use gcloud_sdk::tonic::metadata::MetadataValue;
use gcloud_sdk::tonic::{Extensions, Request};
use gcloud_sdk::{
    GoogleApi, GoogleAuthHeaders, GoogleAuthMiddleware, GoogleAuthMiddlewareLayer, HeaderMap,
    RequestTimeouts,
};
use itertools::Itertools;
use tokio::process::Command;
use tokio::time::timeout;

use crate::domain::DbTarget;
use crate::port::GatewayError;

/// Production Firestore.
const GOOGLE_URL: &str = "https://firestore.googleapis.com";
/// How long fetching credentials may take.
const STEP_TIMEOUT: Duration = Duration::from_secs(10);
/// How long opening the channel may take.
const CHANNEL_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest any single call (a stream included, start to last message) may take.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// The header Google's gRPC frontends route on.
const ROUTING_HEADER: &str = "x-goog-request-params";

/// The `google-cloud-resource-prefix` header value: names the database every request
/// belongs to, so a named database is never silently replaced by `(default)`.
pub fn resource_prefix(t: &DbTarget) -> String {
    t.database_path()
}

/// Where requests go and how they authenticate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// The local emulator at this URL, authenticated as its `owner` user.
    Emulator(String),
    /// Production Firestore, authenticated with Google credentials.
    Google,
}

/// The endpoint chosen by the environment: the emulator when
/// `FIRESTORE_EMULATOR_HOST` is set and non-empty, else production.
pub fn endpoint(env: impl Fn(&str) -> Option<String>) -> Endpoint {
    env("FIRESTORE_EMULATOR_HOST")
        .map(|host| host.trim().to_owned())
        .filter(|host| !host.is_empty())
        .map_or(Endpoint::Google, |host| {
            Endpoint::Emulator(if host.contains("://") {
                host
            } else {
                format!("http://{host}")
            })
        })
}

/// An `x-goog-request-params` entry: `key=value`, percent-encoded the way Google's own
/// clients do (`/` kept).
pub fn routing_param(key: &str, value: &str) -> String {
    let encoded: String = value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    format!("{key}={encoded}")
}

/// Wraps `message` in a request carrying the routing entry `key=value`.
pub fn routed<T>(message: T, key: &str, value: &str) -> Request<T> {
    let mut request = Request::new(message);
    // The entry is percent-encoded ASCII, so this cannot fail; if it somehow did, the
    // `google-cloud-resource-prefix` header still names the database.
    if let Ok(entry) = MetadataValue::try_from(routing_param(key, value)) {
        request.metadata_mut().insert(ROUTING_HEADER, entry);
    }
    request
}

fn other(message: impl Into<String>) -> GatewayError {
    GatewayError::Other(message.into())
}

/// The error and its causes, innermost last: `transport error: tcp connect error: ...`.
fn error_chain(e: &(dyn std::error::Error + 'static)) -> String {
    std::iter::successors(Some(e), |e| e.source())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

/// A fixed `Authorization: Bearer` header; the token never appears in `Debug` output.
struct StaticBearer {
    tag: EntityTag,
    headers: HeaderMap,
}

impl StaticBearer {
    fn new(token: &str) -> Result<Self, GatewayError> {
        let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| GatewayError::Auth("the access token is not a valid header".to_owned()))?;
        value.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(
            gcloud_sdk::tonic::codegen::http::header::AUTHORIZATION,
            value,
        );
        Ok(Self {
            tag: EntityTag::new(),
            headers,
        })
    }

    fn into_auth_headers(self) -> GoogleAuthHeaders {
        GoogleAuthHeaders::from(Credentials::from(self))
    }
}

impl fmt::Debug for StaticBearer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticBearer").finish_non_exhaustive()
    }
}

// The trait declares `impl Future`; the bodies have nothing to await.
#[allow(clippy::unused_async_trait_impl)]
impl CredentialsProvider for StaticBearer {
    async fn headers(
        &self,
        extensions: Extensions,
    ) -> Result<CacheableResource<HeaderMap>, CredentialsError> {
        Ok(if extensions.get::<EntityTag>() == Some(&self.tag) {
            CacheableResource::NotModified
        } else {
            CacheableResource::New {
                entity_tag: self.tag.clone(),
                data: self.headers.clone(),
            }
        })
    }

    async fn universe_domain(&self) -> Option<String> {
        None
    }
}

/// Why a credential source produced no token.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CredentialFailure {
    reason: String,
    /// The failure looks like a network problem (timeout, unreachable token endpoint)
    /// rather than a rejected or missing login.
    network: bool,
}

impl CredentialFailure {
    fn rejected(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            network: false,
        }
    }
}

/// Runs `step` for at most [`STEP_TIMEOUT`].
async fn within<T>(
    step: impl Future<Output = Result<T, CredentialFailure>>,
) -> Result<T, CredentialFailure> {
    timeout(STEP_TIMEOUT, step).await.unwrap_or_else(|_| {
        Err(CredentialFailure {
            reason: format!("timed out after {}s", STEP_TIMEOUT.as_secs()),
            network: true,
        })
    })
}

/// Application Default Credentials, with a token fetched up front so a missing or
/// expired login is reported here, as an auth failure, rather than inside the first call.
async fn adc_headers() -> Result<GoogleAuthHeaders, CredentialFailure> {
    let headers = GoogleAuthHeaders::from_adc()
        .await
        .map_err(|e| CredentialFailure::rejected(e.to_string()))?;
    headers.headers().await.map_err(|e| CredentialFailure {
        network: matches!(e.kind(), ErrorKind::Credentials(c) if c.is_transient()),
        reason: e.to_string(),
    })?;
    Ok(headers)
}

/// The token `gcloud auth print-access-token` prints, fetched once.
async fn gcloud_headers() -> Result<GoogleAuthHeaders, CredentialFailure> {
    let out = Command::new("gcloud")
        .args(["auth", "print-access-token"])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| CredentialFailure::rejected(format!("cannot run gcloud: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // gcloud's explanation is the `ERROR:` line; the rest is warnings and how-tos.
        let reason = stderr
            .lines()
            .find(|line| line.starts_with("ERROR"))
            .or_else(|| stderr.lines().next())
            .unwrap_or("no output");
        return Err(CredentialFailure::rejected(format!(
            "`gcloud auth print-access-token` failed: {}",
            reason.trim()
        )));
    }
    let token = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if token.is_empty() {
        return Err(CredentialFailure::rejected(
            "`gcloud auth print-access-token` printed no token",
        ));
    }
    StaticBearer::new(&token)
        .map(StaticBearer::into_auth_headers)
        .map_err(|e| CredentialFailure::rejected(e.to_string()))
}

/// The conventional place `gcloud auth application-default login` writes to.
fn well_known_adc_file(env: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let non_empty = |name: &str| env(name).filter(|v| !v.trim().is_empty());
    non_empty("CLOUDSDK_CONFIG")
        .map(PathBuf::from)
        .or_else(|| non_empty("HOME").map(|home| PathBuf::from(home).join(".config/gcloud")))
        .map(|dir| dir.join("application_default_credentials.json"))
}

/// Whether Application Default Credentials can exist here: a key file named by
/// `GOOGLE_APPLICATION_CREDENTIALS`, the well-known login file, or a Google-hosted
/// environment (whose metadata server is not probed). When none does, trying ADC would
/// only wait on a metadata server that is not there.
fn adc_available(
    env: impl Fn(&str) -> Option<String>,
    is_file: impl Fn(&Path) -> bool,
    on_gce: bool,
) -> bool {
    let set = |name: &str| env(name).is_some_and(|v| !v.trim().is_empty());
    on_gce
        || [
            "GOOGLE_APPLICATION_CREDENTIALS",
            "GCE_METADATA_HOST",
            "K_SERVICE",
        ]
        .into_iter()
        .any(set)
        || well_known_adc_file(&env).is_some_and(|file| is_file(&file))
}

/// Whether this machine reports itself as Google-hosted (DMI product name).
fn on_gce() -> bool {
    std::fs::read_to_string("/sys/class/dmi/id/product_name")
        .is_ok_and(|name| name.trim().starts_with("Google"))
}

/// The error for credential sources that all failed: [`GatewayError::Auth`], unless one
/// failed for network reasons, in which case the login may be fine and it is
/// [`GatewayError::Other`].
fn credentials_error(attempts: &[(&str, CredentialFailure)]) -> GatewayError {
    let detail = attempts
        .iter()
        .map(|(source, failure)| format!("{source}: {}", failure.reason))
        .join("; ");
    if attempts.iter().any(|(_, failure)| failure.network) {
        other(format!(
            "could not reach Google to get credentials ({detail})"
        ))
    } else {
        GatewayError::Auth(format!("no usable Google credentials ({detail})"))
    }
}

/// Google credentials: Application Default Credentials where they can exist, else (or
/// if they fail) a `gcloud` access token.
async fn google_headers() -> Result<GoogleAuthHeaders, GatewayError> {
    let adc_possible = adc_available(|name| std::env::var(name).ok(), Path::is_file, on_gce());
    let adc = if adc_possible {
        match within(adc_headers()).await {
            Ok(headers) => return Ok(headers),
            Err(failure) => Some(failure),
        }
    } else {
        None
    };
    within(gcloud_headers()).await.map_err(|gcloud| {
        let attempts = adc
            .map(|f| ("Application Default Credentials", f))
            .into_iter()
            .chain([("gcloud", gcloud)])
            .collect::<Vec<_>>();
        credentials_error(&attempts)
    })
}

/// Opens the data client for `t` on `endpoint`. Every request it makes carries
/// `google-cloud-resource-prefix: projects/P/databases/D`, and no call (stream
/// included) may take longer than [`CALL_TIMEOUT`].
///
/// # Errors
/// [`GatewayError::Auth`] if no credentials work; [`GatewayError::Other`] if Firestore
/// (or Google's token endpoint) cannot be reached. Takes at most 25 seconds: ADC and
/// `gcloud` get ten each, the channel five.
pub async fn open(
    t: &DbTarget,
    endpoint: &Endpoint,
) -> Result<GoogleApi<FirestoreClient<GoogleAuthMiddleware>>, GatewayError> {
    let (url, auth) = match endpoint {
        Endpoint::Emulator(url) => (
            url.as_str(),
            StaticBearer::new("owner")?.into_auth_headers(),
        ),
        Endpoint::Google => (GOOGLE_URL, google_headers().await?),
    };
    let layer = GoogleAuthMiddlewareLayer::new(auth, Some(resource_prefix(t)))
        .map_err(|e| other(format!("cannot set up Firestore headers: {e}")))?;
    timeout(
        CHANNEL_TIMEOUT,
        GoogleApi::from_function_with_middleware(FirestoreClient::new, url, layer),
    )
    .await
    .map_err(|_| other(format!("timed out connecting to Firestore at {url}")))?
    .map(|api| api.with_request_timeouts(RequestTimeouts::new().with_total(CALL_TIMEOUT)))
    .map_err(|e| {
        let detail = match e.kind() {
            ErrorKind::GrpcStatus(transport) => error_chain(transport),
            _ => e.to_string(),
        };
        other(format!("cannot connect to Firestore at {url}: {detail}"))
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::grpc::test_util::target;

    #[test]
    fn resource_prefix_is_database_path() {
        assert_eq!(resource_prefix(&target("p", "d")), "projects/p/databases/d");
    }

    #[test]
    fn emulator_host_selects_a_plain_http_endpoint() {
        let env = |v: &str| (v == "FIRESTORE_EMULATOR_HOST").then(|| "127.0.0.1:8080".to_owned());
        assert_eq!(
            endpoint(env),
            Endpoint::Emulator("http://127.0.0.1:8080".to_owned())
        );
    }

    #[test]
    fn unset_or_blank_emulator_host_means_production() {
        assert_eq!(endpoint(|_| None), Endpoint::Google);
        assert_eq!(endpoint(|_| Some("  ".to_owned())), Endpoint::Google);
    }

    #[test]
    fn routing_param_keeps_slashes_and_encodes_the_rest() {
        assert_eq!(
            routing_param("database", "projects/p/databases/d"),
            "database=projects/p/databases/d"
        );
        assert_eq!(
            routing_param("database", "projects/p/databases/(default)"),
            "database=projects/p/databases/%28default%29"
        );
    }

    #[test]
    fn routed_requests_carry_the_database_routing_entry() {
        let request = routed((), "database", "projects/p/databases/jobs-db");
        assert_eq!(
            request.metadata().get("x-goog-request-params").unwrap(),
            "database=projects/p/databases/jobs-db"
        );
    }

    #[tokio::test]
    async fn static_bearer_serves_its_header_once_per_tag_and_hides_the_token() {
        let bearer = StaticBearer::new("s3cret").unwrap();
        assert!(!format!("{bearer:?}").contains("s3cret"));
        let CacheableResource::New { entity_tag, data } =
            bearer.headers(Extensions::new()).await.unwrap()
        else {
            panic!("first call must serve the headers");
        };
        assert_eq!(data["authorization"], "Bearer s3cret");
        assert!(data["authorization"].is_sensitive());
        let mut seen = Extensions::new();
        seen.insert(entity_tag);
        assert!(matches!(
            bearer.headers(seen).await.unwrap(),
            CacheableResource::NotModified
        ));
    }

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn adc_is_available_with_a_key_file_variable_the_login_file_or_gce() {
        let none = |_: &Path| false;
        assert!(!adc_available(env(&[("HOME", "/home/u")]), none, false));
        assert!(adc_available(
            env(&[("GOOGLE_APPLICATION_CREDENTIALS", "/k.json")]),
            none,
            false
        ));
        assert!(adc_available(env(&[]), none, true));
        assert!(adc_available(env(&[("K_SERVICE", "svc")]), none, false));
        assert!(!adc_available(
            env(&[("GOOGLE_APPLICATION_CREDENTIALS", " ")]),
            none,
            false
        ));
    }

    #[test]
    fn adc_login_file_is_looked_up_under_cloudsdk_config_else_home() {
        let only = |expected: &'static str| move |p: &Path| p == Path::new(expected);
        assert!(adc_available(
            env(&[("HOME", "/home/u")]),
            only("/home/u/.config/gcloud/application_default_credentials.json"),
            false
        ));
        assert!(adc_available(
            env(&[("HOME", "/home/u"), ("CLOUDSDK_CONFIG", "/cfg")]),
            only("/cfg/application_default_credentials.json"),
            false
        ));
        // HOME is ignored once CLOUDSDK_CONFIG is set.
        assert!(!adc_available(
            env(&[("HOME", "/home/u"), ("CLOUDSDK_CONFIG", "/cfg")]),
            only("/home/u/.config/gcloud/application_default_credentials.json"),
            false
        ));
    }

    #[test]
    fn credential_failures_are_auth_unless_one_was_a_network_problem() {
        let rejected = CredentialFailure::rejected("no account");
        let network = CredentialFailure {
            reason: "timed out after 10s".to_owned(),
            network: true,
        };
        assert!(matches!(
            credentials_error(&[("gcloud", rejected.clone())]),
            GatewayError::Auth(m) if m.contains("no account")
        ));
        assert!(matches!(
            credentials_error(&[("Application Default Credentials", network), ("gcloud", rejected)]),
            GatewayError::Other(m) if m.contains("timed out") && m.contains("no account")
        ));
    }
}
