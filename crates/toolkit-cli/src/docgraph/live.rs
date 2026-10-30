//! The live AWS client of `toolkit documentdb wa-review` (S2, #5489;
//! R-DG-04, design `docs/migration/docgraph.md`).
//!
//! [`LiveAwsClient`] implements the review's [`AwsClient`] seam for exactly
//! the ten read-only operations of [`OPERATIONS`]. Each call:
//!
//! 1. maps the seam request (the `PascalCase` JSON the checks build) onto the
//!    official service crate's typed input, refusing any other operation,
//!    member or type before anything is signed or sent;
//! 2. lets the generated SDK orchestrator encode it (Query, AWS JSON or
//!    RPC-v2 CBOR) and hand the request to this module's HTTP connector, the
//!    bridge. The SDK is configured with no credentials provider, `noAuth`
//!    preferred, an identity cache that refuses any AWS credential identity,
//!    its own retries, timeouts and stalled-stream protection off, and this
//!    connector as its only HTTP client;
//! 3. in the bridge: refuses a request that is not for the operation's own
//!    standard endpoint, that carries an SDK-prepared signature or a
//!    streaming body, removes the generated framing headers
//!    ([`FRAMING_HEADERS`]), signs the final body bytes with the toolkit
//!    signer under the operation's one absolute deadline, and sends it through
//!    the toolkit [`StreamingTransport`] under the same deadline and a 16 MiB
//!    response cap. The transport regenerates authority and framing from the
//!    unchanged URL and body;
//! 4. refuses document type declarations, then hands the bounded raw
//!    response, non-2xx included, back to the SDK's
//!    modeled decoder and maps the decoded output onto the seam: only the
//!    fields the checks read, absent values omitted, never `null`.
//!
//! The toolkit is the sole signer, credential resolver and retry owner: the
//! shared [`RetryPolicy`] (three retries, four sends, full jitter, 20 s cap)
//! wraps whole attempts, each attempt builds and signs one fresh request, and
//! the backoff never sleeps past the operation deadline. A service error keeps
//! only its status and sanitized code, never its message or request id.
//!
//! Runtime: the SDK future is polled on the calling thread with no async
//! runtime. Its only wait is the bridge connector, which parks the generated
//! request in a slot and returns `Pending`; the driver then runs the
//! synchronous toolkit signer and transport (which use their own
//! `SdkRuntime`), fills the response slot and polls again. Nothing is nested
//! inside another runtime, and an SDK wait on anything else fails closed.
//!
//! This build does not route `--live` to this client: the CLI still refuses
//! live mode until the exact-source security review of this slice.

#![allow(clippy::module_name_repetitions)]

use std::future::Future;
use std::pin::pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use aws_smithy_runtime_api::box_error::BoxError;
use aws_smithy_runtime_api::client::auth::AuthSchemeId;
use aws_smithy_runtime_api::client::http::{
    http_client_fn, HttpConnector, HttpConnectorFuture, SharedHttpClient, SharedHttpConnector,
};
use aws_smithy_runtime_api::client::identity::{
    Identity, IdentityFuture, ResolveCachedIdentity, ResolveIdentity, SharedIdentityResolver,
};
use aws_smithy_runtime_api::client::orchestrator::{
    HttpRequest as SdkHttpRequest, HttpResponse as SdkHttpResponse,
};
use aws_smithy_runtime_api::client::result::{ConnectorError, SdkError};
use aws_smithy_runtime_api::client::runtime_components::RuntimeComponents;
use aws_smithy_runtime_api::http::StatusCode;
use aws_smithy_types::body::SdkBody;
use aws_smithy_types::config_bag::ConfigBag;
use aws_smithy_types::date_time::Format;
use aws_smithy_types::error::metadata::ProvideErrorMetadata;
use aws_smithy_types::retry::RetryConfig;
use aws_smithy_types::timeout::TimeoutConfig;
use aws_smithy_types::DateTime;
use toolkit_aws::error::TimeoutPhase;
use toolkit_aws::sdk::source::GuardedCredentials;
use toolkit_aws::sdk::SigV4Signer;
use toolkit_aws::{
    is_valid_region, run_with_retry, AwsError, Headers, HttpRequest, Jitter, Method, RetryPolicy,
    ServiceError, Signer, SigningScope, Sleeper, StreamError, StreamLimits, StreamingTransport,
    Timeouts, FRAMING_HEADERS,
};
use toolkit_core::Json;

use super::client::{AwsClient, Service};

/// One allowed review operation: the seam pair and its wire facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Operation {
    pub(crate) service: Service,
    pub(crate) name: &'static str,
    /// The SigV4 signing name (not the CLI name: DocumentDB signs as `rds`,
    /// CloudWatch as `monitoring`).
    pub(crate) signing_name: &'static str,
    host_prefix: &'static str,
    path: &'static str,
}

const QUERY_PATH: &str = "/";

const fn docdb(name: &'static str) -> Operation {
    Operation {
        service: Service::Docdb,
        name,
        signing_name: "rds",
        host_prefix: "rds",
        path: QUERY_PATH,
    }
}

/// The ten allowed service/operation pairs (R-DG-04). Anything else fails
/// before a request is built, signed or sent.
pub(crate) const OPERATIONS: [Operation; 10] = [
    docdb("DescribeDBClusters"),
    docdb("DescribeDBInstances"),
    docdb("DescribeEvents"),
    docdb("DescribeDBClusterParameters"),
    docdb("DescribeDBSubnetGroups"),
    docdb("ListTagsForResource"),
    Operation {
        service: Service::CloudWatch,
        name: "GetMetricStatistics",
        signing_name: "monitoring",
        host_prefix: "monitoring",
        path: "/service/GraniteServiceVersion20100801/operation/GetMetricStatistics",
    },
    Operation {
        service: Service::CloudWatch,
        name: "DescribeAlarms",
        signing_name: "monitoring",
        host_prefix: "monitoring",
        path: "/service/GraniteServiceVersion20100801/operation/DescribeAlarms",
    },
    Operation {
        service: Service::Ec2,
        name: "DescribeSecurityGroups",
        signing_name: "ec2",
        host_prefix: "ec2",
        path: QUERY_PATH,
    },
    Operation {
        service: Service::SecretsManager,
        name: "ListSecrets",
        signing_name: "secretsmanager",
        host_prefix: "secretsmanager",
        path: QUERY_PATH,
    },
];

/// The allowed operation `name` of `service`, if any.
pub(crate) fn operation(service: Service, name: &str) -> Option<&'static Operation> {
    OPERATIONS
        .iter()
        .find(|op| op.service == service && op.name == name)
}

impl Operation {
    /// The operation's own standard-partition regional endpoint and path.
    fn url(&self, region: &str) -> String {
        format!(
            "https://{}.{region}.amazonaws.com{}",
            self.host_prefix, self.path
        )
    }

    fn scope(&self, region: &str) -> SigningScope {
        SigningScope {
            region: region.to_string(),
            service: self.signing_name.to_string(),
        }
    }

    /// The root element a Query/XML response must have (`{Op}Response`).
    fn xml_root(&self) -> Option<String> {
        matches!(self.service, Service::Docdb | Service::Ec2)
            .then(|| format!("{}Response", self.name))
    }
}

/// Signs one request with credentials bounded by the operation deadline.
pub trait DeadlineSigner {
    /// Sign `request` for `scope`; credential resolution must stop at
    /// `deadline` (R-DG-07).
    ///
    /// # Errors
    /// The fixed credential or signing failure; nothing is sent after it.
    fn sign_until(
        &self,
        request: &mut HttpRequest,
        scope: &SigningScope,
        deadline: Instant,
    ) -> Result<(), AwsError>;
}

/// Production signing: the toolkit SigV4 signer over the guarded credential
/// source, bounded by the operation deadline
/// ([`GuardedCredentials::until`]).
impl DeadlineSigner for GuardedCredentials {
    fn sign_until(
        &self,
        request: &mut HttpRequest,
        scope: &SigningScope,
        deadline: Instant,
    ) -> Result<(), AwsError> {
        SigV4Signer::new(self.until(deadline)).sign(request, scope)
    }
}

/// Per-operation bounds (R-DG-07).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewBounds {
    /// The absolute deadline of one operation from its start: credentials,
    /// signing, DNS, connect, TLS, every attempt, backoff and body reads.
    pub operation: Duration,
    /// The largest response body read for one attempt.
    pub max_response_bytes: u64,
    /// Connect and idle-read budgets of each attempt.
    pub timeouts: Timeouts,
}

impl Default for ReviewBounds {
    /// 60 s per operation, 16 MiB per response, 10 s connect and 30 s idle.
    fn default() -> Self {
        Self {
            operation: Duration::from_secs(60),
            max_response_bytes: 16 * 1024 * 1024,
            timeouts: Timeouts::new(Duration::from_secs(10), Duration::from_secs(30))
                .unwrap_or_default(),
        }
    }
}

/// Chunk size of the bounded response read.
const CHUNK_BYTES: usize = 64 * 1024;

/// The live review client.
pub struct LiveAwsClient<'a> {
    region: String,
    transport: &'a dyn StreamingTransport,
    signer: &'a dyn DeadlineSigner,
    sleeper: &'a dyn Sleeper,
    jitter: &'a dyn Jitter,
    bounds: ReviewBounds,
}

impl<'a> LiveAwsClient<'a> {
    /// A client for `region` over the toolkit `transport` and `signer`, with
    /// the shared retry policy's backoff slept by `sleeper` and jittered by
    /// `jitter`.
    ///
    /// # Errors
    /// `InvalidConfig` (field `region`) for a malformed region or one outside
    /// the standard AWS partition.
    pub fn new(
        region: &str,
        transport: &'a dyn StreamingTransport,
        signer: &'a dyn DeadlineSigner,
        sleeper: &'a dyn Sleeper,
        jitter: &'a dyn Jitter,
    ) -> Result<Self, AwsError> {
        if !is_valid_region(region) {
            return Err(AwsError::invalid_config(
                "region",
                "the region must look like us-east-1",
            ));
        }
        if ["cn-", "us-gov-", "us-iso", "eu-isoe-"]
            .iter()
            .any(|prefix| region.starts_with(prefix))
        {
            return Err(AwsError::invalid_config(
                "region",
                "the live review client supports standard AWS partition regions only",
            ));
        }
        Ok(Self {
            region: region.to_string(),
            transport,
            signer,
            sleeper,
            jitter,
            bounds: ReviewBounds::default(),
        })
    }

    /// Replace the per-operation bounds.
    #[must_use]
    pub fn with_bounds(mut self, bounds: ReviewBounds) -> Self {
        self.bounds = bounds;
        self
    }

    /// One attempt: one SDK encoding, one toolkit signing, one send.
    fn attempt(
        &self,
        op: &Operation,
        prepared: &Prepared,
        deadline: Instant,
    ) -> Result<Json, AwsError> {
        if Instant::now() >= deadline {
            return Err(AwsError::timeout(
                TimeoutPhase::Deadline,
                "the operation deadline elapsed before the next attempt; nothing more was sent",
            ));
        }
        let exchange = Exchange::default();
        let outcome = drive(
            send(prepared, &self.region, exchange.http_client()),
            &exchange,
            |request| self.serve(op, &request, deadline),
        )?;
        match outcome {
            Ok(json) => Ok(json),
            // A bridge failure reaches the SDK as a dispatch failure; the
            // toolkit's own typed error is the one reported.
            Err(error) => Err(exchange.take_failure().unwrap_or(error)),
        }
    }

    /// The bridge: admit, sign and send one generated request; return the
    /// bounded raw response for the modeled decoder.
    fn serve(
        &self,
        op: &Operation,
        generated: &SdkHttpRequest,
        deadline: Instant,
    ) -> Result<SdkHttpResponse, AwsError> {
        let mut request = toolkit_request(op, &self.region, generated, self.bounds.timeouts)?;
        self.signer
            .sign_until(&mut request, &op.scope(&self.region), deadline)?;
        let limits =
            StreamLimits::new(self.bounds.max_response_bytes, CHUNK_BYTES)?.with_deadline(deadline);
        let (status, headers, body) = {
            let mut stream = self.transport.open(&request, &limits)?;
            let mut body = Vec::new();
            loop {
                match stream.next_chunk() {
                    Ok(Some(chunk)) => body.extend(chunk),
                    Ok(None) => break,
                    Err(error) => return Err(stream_error(error)),
                }
            }
            (stream.status(), stream.headers().clone(), body)
        };
        // Refuse a real declaration for every operation and HTTP status,
        // before any modeled success or service-error decoder sees it.
        if has_document_type(&body) {
            return Err(AwsError::invalid_response(
                "the service returned an XML document type declaration",
            ));
        }
        if (200..300).contains(&status) {
            if body.is_empty() {
                return Err(AwsError::invalid_response(
                    "the service returned an empty successful response",
                ));
            }
            if let Some(root) = op.xml_root() {
                if !well_formed_xml(&body, &root) {
                    return Err(AwsError::invalid_response(
                        "the service returned a malformed XML response",
                    ));
                }
            }
        }
        sdk_response(status, &headers, body)
    }
}

impl AwsClient for LiveAwsClient<'_> {
    fn call(
        &mut self,
        service: Service,
        operation_name: &str,
        request: &Json,
    ) -> Result<Json, AwsError> {
        let op = operation(service, operation_name).ok_or_else(|| {
            AwsError::invalid_config(
                "operation",
                "the live review client sends only its ten read-only review operations",
            )
        })?;
        let prepared = Prepared::parse(op, request)?;
        let deadline = Instant::now() + self.bounds.operation;
        let sleeper = DeadlineSleeper {
            inner: self.sleeper,
            deadline,
        };
        run_with_retry(&RetryPolicy::default(), &sleeper, self.jitter, |_| {
            self.attempt(op, &prepared, deadline)
        })
        .map(|attempted| attempted.value)
        .map_err(|failure| failure.error)
    }
}

/// A backoff that never sleeps past the operation deadline; the next attempt
/// then refuses to start.
struct DeadlineSleeper<'s> {
    inner: &'s dyn Sleeper,
    deadline: Instant,
}

impl Sleeper for DeadlineSleeper<'_> {
    fn sleep(&self, duration: Duration) {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        self.inner.sleep(duration.min(remaining));
    }
}

/// Whether an XML body contains a real document type declaration (#5544).
///
/// roxmltree 0.14.1's default DTD refusal covers `DtdStart`, but silently
/// accepts `EmptyDtd` (plain, SYSTEM or PUBLIC without an internal subset).
/// Check both tokens explicitly, using its same pinned tokenizer. Comments,
/// CDATA and processing instructions are distinct tokens, never searched as
/// text. This only detects declarations: non-XML/invalid UTF-8 bodies remain
/// subject to the operation's existing validation and modeled decoder.
fn has_document_type(body: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(body) else {
        return false;
    };
    xmlparser::Tokenizer::from(text).any(|token| {
        matches!(
            token,
            Ok(xmlparser::Token::DtdStart { .. } | xmlparser::Token::EmptyDtd { .. })
        )
    })
}

/// Whole-document validation of a successful Query/XML response (#5521).
///
/// The pinned SDK decoder reads only the elements it looks for: it does not
/// check that closes match their opens, that there is one root, or that
/// nothing follows it. So the body must first parse as one well-formed XML
/// document (UTF-8; matched nesting; a single root; only comments, processing
/// instructions and whitespace around it; declared namespace prefixes)
/// whose root element is the operation's own
/// `{Op}Response`, in any namespace. The parser is `roxmltree` (already a
/// toolkit-cli dependency, over the same `xmlparser` 0.13.6 tokenizer as the
/// SDK's `aws-smithy-xml`). [`has_document_type`] explicitly refuses DTDs
/// before this check; the parser's defaults do not refuse all DTD forms.
///
/// roxmltree 0.14.1 itself accepts end of input while elements are still
/// open (it refuses only a mismatched close), so every element must also be
/// closed: see [`element_closed`]. With closes matched, that is the same as
/// the root closing.
///
/// It only validates: the SDK decoder still reads the unchanged bytes, so a
/// valid document decodes exactly as before.
fn well_formed_xml(body: &[u8], root: &str) -> bool {
    let Ok(text) = std::str::from_utf8(body) else {
        return false;
    };
    let Ok(document) = roxmltree::Document::parse(text) else {
        return false;
    };
    document.root_element().tag_name().name() == root
        && document
            .descendants()
            .filter(roxmltree::Node::is_element)
            .all(|element| element_closed(text, element))
}

/// Whether `element` was closed. roxmltree spans an element from its start
/// tag to its close tag, or to the end of its start tag while unclosed. A
/// start tag cannot contain `<` (not even in an attribute value), so the span
/// of a closed element ends with `/>` (empty) or contains `</` (its close).
fn element_closed(text: &str, element: roxmltree::Node<'_, '_>) -> bool {
    text.get(element.range())
        .is_some_and(|markup| markup.ends_with("/>") || markup.contains("</"))
}

fn stream_error(error: StreamError) -> AwsError {
    match error {
        StreamError::TooLarge => {
            AwsError::invalid_response("the service response exceeded its size limit")
        }
        StreamError::IdleTimeout => AwsError::timeout(
            TimeoutPhase::Read,
            "no response bytes arrived within the read timeout",
        ),
        StreamError::DeadlineExceeded => AwsError::timeout(
            TimeoutPhase::Deadline,
            "the operation deadline elapsed while reading the response",
        ),
        StreamError::Failed(error) => error,
    }
}

fn sdk_response(
    status: u16,
    headers: &Headers,
    body: Vec<u8>,
) -> Result<SdkHttpResponse, AwsError> {
    let invalid = || AwsError::invalid_response("the service response head could not be read");
    let status = StatusCode::try_from(status).map_err(|_| invalid())?;
    let mut response = SdkHttpResponse::new(status, SdkBody::from(body));
    for (name, value) in headers.entries() {
        response
            .headers_mut()
            .try_insert(name.to_string(), value.to_string())
            .map_err(|_| invalid())?;
    }
    Ok(response)
}

/// Headers whose presence means the SDK, not the toolkit, prepared a
/// signature: `noAuth` was not selected, so the bridge fails closed.
const SDK_SIGNING_HEADERS: [&str; 4] = [
    "authorization",
    "x-amz-date",
    "x-amz-security-token",
    "x-amz-content-sha256",
];

/// The toolkit request for one SDK-generated request (T-DG-04e): the URL must
/// be exactly `op`'s endpoint, the body buffered, no SDK signature present;
/// generated framing headers are removed, every other header kept unchanged,
/// and the final body bytes kept as they are.
pub(crate) fn toolkit_request(
    op: &Operation,
    region: &str,
    generated: &SdkHttpRequest,
    timeouts: Timeouts,
) -> Result<HttpRequest, AwsError> {
    let url = op.url(region);
    if generated.uri() != url {
        return Err(AwsError::invalid_config(
            "url",
            "the generated request does not name the operation's own endpoint",
        ));
    }
    if generated.method() != "POST" {
        return Err(AwsError::invalid_config(
            "method",
            "the review operations are sent only as POST",
        ));
    }
    let body = generated.body().bytes().ok_or_else(|| {
        AwsError::invalid_config(
            "request",
            "a streaming request body cannot be signed as final bytes; it is not sent",
        )
    })?;
    let mut headers = Headers::new();
    for (name, value) in generated.headers() {
        let lower = name.to_ascii_lowercase();
        if SDK_SIGNING_HEADERS.contains(&lower.as_str()) {
            return Err(AwsError::invalid_config(
                "signing",
                "the SDK prepared a signature; only the toolkit signs review requests",
            ));
        }
        if FRAMING_HEADERS.contains(&lower.as_str()) {
            continue;
        }
        headers.insert(name, value)?;
    }
    Ok(HttpRequest {
        method: Method::Post,
        url,
        headers,
        body: body.to_vec(),
        timeouts,
    })
}

// ---- the bridge connector and its driver ----

#[derive(Default)]
struct Slot {
    request: Option<SdkHttpRequest>,
    response: Option<Result<SdkHttpResponse, ConnectorError>>,
    failure: Option<AwsError>,
    calls: usize,
}

/// The SDK's only HTTP connector: it parks the generated request and waits
/// for the driver to answer it. One request per attempt.
#[derive(Clone, Default)]
struct Exchange(Arc<Mutex<Slot>>);

impl std::fmt::Debug for Exchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Exchange")
    }
}

impl Exchange {
    fn slot(&self) -> MutexGuard<'_, Slot> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn http_client(&self) -> SharedHttpClient {
        let connector = self.clone();
        http_client_fn(move |_, _| SharedHttpConnector::new(connector.clone()))
    }

    fn take_request(&self) -> Option<SdkHttpRequest> {
        self.slot().request.take()
    }

    fn answer(&self, reply: Result<SdkHttpResponse, AwsError>) {
        let mut slot = self.slot();
        let response = match reply {
            Ok(response) => Ok(response),
            Err(error) => {
                slot.failure = Some(error);
                Err(ConnectorError::other(
                    "the toolkit transport did not complete the request".into(),
                    None,
                ))
            }
        };
        slot.response = Some(response);
    }

    fn take_failure(&self) -> Option<AwsError> {
        self.slot().failure.take()
    }
}

impl HttpConnector for Exchange {
    fn call(&self, request: SdkHttpRequest) -> HttpConnectorFuture {
        {
            let mut slot = self.slot();
            slot.calls += 1;
            if slot.calls > 1 {
                return HttpConnectorFuture::ready(Err(ConnectorError::other(
                    "the SDK made a second request in one attempt".into(),
                    None,
                )));
            }
            slot.request = Some(request);
        }
        let exchange = self.clone();
        HttpConnectorFuture::new(std::future::poll_fn(move |_| {
            match exchange.slot().response.take() {
                Some(reply) => Poll::Ready(reply),
                None => Poll::Pending,
            }
        }))
    }
}

/// Poll `future` on this thread with no runtime. Whenever it waits, the
/// bridge must hold a request: `serve` answers it and polling resumes.
fn drive<F: Future>(
    future: F,
    exchange: &Exchange,
    mut serve: impl FnMut(SdkHttpRequest) -> Result<SdkHttpResponse, AwsError>,
) -> Result<F::Output, AwsError> {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return Ok(output);
        }
        let Some(request) = exchange.take_request() else {
            return Err(AwsError::invalid_config(
                "sdk",
                "the SDK waited on something other than the toolkit transport; nothing more is sent",
            ));
        };
        exchange.answer(serve(request));
    }
}

/// The identity cache installed in every client: identities pass through
/// uncached, and an AWS credential identity is refused, so only the `noAuth`
/// identity can reach the SDK's (disabled) signing step.
#[derive(Debug)]
struct RefuseCredentials;

/// Refuse an SDK identity that carries AWS credentials (T-DG-04e).
///
/// # Errors
/// A fixed refusal naming no identity value.
pub(crate) fn refuse_credential_identity(identity: &Identity) -> Result<(), BoxError> {
    if identity
        .data::<aws_credential_types::Credentials>()
        .is_some()
    {
        return Err(
            "an SDK credential identity was resolved; only the toolkit resolves credentials".into(),
        );
    }
    Ok(())
}

impl ResolveCachedIdentity for RefuseCredentials {
    fn resolve_cached_identity<'a>(
        &'a self,
        resolver: SharedIdentityResolver,
        components: &'a RuntimeComponents,
        config: &'a ConfigBag,
    ) -> IdentityFuture<'a> {
        IdentityFuture::new(async move {
            let identity = resolver.resolve_identity(components, config).await?;
            refuse_credential_identity(&identity)?;
            Ok(identity)
        })
    }
}

/// A generated client configured for the bridge.
// recorded-shape: builds an SDK client from fixed configuration; it reads no JSON.
macro_rules! sdk_client {
    ($krate:ident, $region:expr, $http:expr) => {
        $krate::Client::from_conf(
                            $krate::Config::builder()
                                .behavior_version_latest()
                                .region($krate::config::Region::new($region.to_string()))
                                .http_client($http)
                                .identity_cache(RefuseCredentials)
                                .allow_no_auth()
                                .auth_scheme_preference([AuthSchemeId::new("noAuth")])
                                .retry_config(RetryConfig::disabled())
                                .timeout_config(TimeoutConfig::disabled())
                                .stalled_stream_protection(
                                    $krate::config::StalledStreamProtectionConfig::disabled(),
                                )
                                .build(),
                        )
    };
}

/// An SDK failure as a fixed toolkit error: a service error keeps its
/// status and sanitized code only. The Query and JSON decoders report a
/// successful (2xx) response they cannot decode as an unhandled *service*
/// error; that is a malformed response, never a service verdict.
fn classify<E: ProvideErrorMetadata>(error: &SdkError<E, SdkHttpResponse>) -> AwsError {
    match error {
        SdkError::ServiceError(context) if context.raw().status().is_success() => {
            AwsError::invalid_response("a successful service response could not be decoded")
        }
        SdkError::ServiceError(context) => {
            let status = context.raw().status().as_u16();
            AwsError::service(ServiceError::new(
                "POST",
                "/",
                status,
                error.code(),
                "",
                None,
            ))
        }
        SdkError::ResponseError(_) => {
            AwsError::invalid_response("the service response could not be decoded")
        }
        SdkError::ConstructionFailure(_) => {
            AwsError::invalid_config("request", "the review request could not be encoded")
        }
        SdkError::TimeoutError(_) => AwsError::timeout(
            TimeoutPhase::Read,
            "the SDK reported a timeout for the request",
        ),
        _ => AwsError::connect("the request was not completed"),
    }
}

// ---- seam requests ----

/// A seam request validated and typed before any encoding or signing.
enum Prepared {
    Clusters {
        id: Option<String>,
    },
    Instances {
        filters: Option<Vec<(String, Vec<String>)>>,
    },
    Events {
        source_identifier: Option<String>,
        source_type: Option<String>,
        start: Option<DateTime>,
        end: Option<DateTime>,
        marker: Option<String>,
    },
    Parameters {
        group: Option<String>,
    },
    SubnetGroups {
        name: Option<String>,
    },
    Tags {
        resource: Option<String>,
    },
    Metric {
        namespace: Option<String>,
        metric: Option<String>,
        dimensions: Option<Vec<(String, String)>>,
        start: Option<DateTime>,
        end: Option<DateTime>,
        period: Option<i32>,
        statistics: Option<Vec<String>>,
    },
    Alarms {
        prefix: Option<String>,
    },
    Groups {
        ids: Option<Vec<String>>,
    },
    Secrets {
        token: Option<String>,
    },
}

fn unsupported() -> AwsError {
    AwsError::invalid_config(
        "request",
        "the review request has a member the live operation does not map, or one of the wrong type",
    )
}

type Members = [(String, Json)];

// recorded-shape: the seam request the checks build, not a response.
fn members<'j>(request: &'j Json, allowed: &[&str]) -> Result<&'j Members, AwsError> {
    match request {
        Json::Object(pairs) if pairs.iter().all(|(key, _)| allowed.contains(&key.as_str())) => {
            Ok(pairs)
        }
        _ => Err(unsupported()),
    }
}

fn member<'j>(pairs: &'j Members, key: &str) -> Option<&'j Json> {
    pairs
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
}

fn text(pairs: &Members, key: &str) -> Result<Option<String>, AwsError> {
    match member(pairs, key) {
        None => Ok(None),
        Some(Json::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(unsupported()),
    }
}

// recorded-shape: a list in the seam request the checks build, not a response.
fn strings(value: &Json) -> Result<Vec<String>, AwsError> {
    match value {
        Json::Array(items) => items
            .iter()
            .map(|item| item.as_str().map(str::to_string).ok_or_else(unsupported))
            .collect(),
        _ => Err(unsupported()),
    }
}

fn texts(pairs: &Members, key: &str) -> Result<Option<Vec<String>>, AwsError> {
    member(pairs, key).map(strings).transpose()
}

fn time(pairs: &Members, key: &str) -> Result<Option<DateTime>, AwsError> {
    text(pairs, key)?
        .map(|value| DateTime::from_str(&value, Format::DateTime).map_err(|_| unsupported()))
        .transpose()
}

fn int(pairs: &Members, key: &str) -> Result<Option<i32>, AwsError> {
    match member(pairs, key) {
        None => Ok(None),
        Some(Json::Int(value)) => i32::try_from(*value).map(Some).map_err(|_| unsupported()),
        Some(_) => Err(unsupported()),
    }
}

/// An array of objects with exactly the string member `first` and the member
/// `second` read by `read`.
// recorded-shape: an object list in the seam request, not a response.
fn pairs_of<T>(
    pairs: &Members,
    key: &str,
    first: &str,
    second: &str,
    read: impl Fn(&Json) -> Result<T, AwsError>,
) -> Result<Option<Vec<(String, T)>>, AwsError> {
    let Some(value) = member(pairs, key) else {
        return Ok(None);
    };
    let Json::Array(items) = value else {
        return Err(unsupported());
    };
    items
        .iter()
        .map(|item| {
            let entry = members(item, &[first, second])?;
            let name = text(entry, first)?.ok_or_else(unsupported)?;
            let other = member(entry, second).ok_or_else(unsupported)?;
            Ok((name, read(other)?))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

impl Prepared {
    fn parse(op: &Operation, request: &Json) -> Result<Self, AwsError> {
        Ok(match op.name {
            "DescribeDBClusters" => {
                let pairs = members(request, &["DBClusterIdentifier"])?;
                Self::Clusters {
                    id: text(pairs, "DBClusterIdentifier")?,
                }
            }
            "DescribeDBInstances" => {
                let pairs = members(request, &["Filters"])?;
                Self::Instances {
                    filters: pairs_of(pairs, "Filters", "Name", "Values", strings)?,
                }
            }
            "DescribeEvents" => {
                let pairs = members(
                    request,
                    &[
                        "SourceIdentifier",
                        "SourceType",
                        "StartTime",
                        "EndTime",
                        "Marker",
                    ],
                )?;
                Self::Events {
                    source_identifier: text(pairs, "SourceIdentifier")?,
                    source_type: text(pairs, "SourceType")?,
                    start: time(pairs, "StartTime")?,
                    end: time(pairs, "EndTime")?,
                    marker: text(pairs, "Marker")?,
                }
            }
            "DescribeDBClusterParameters" => {
                let pairs = members(request, &["DBClusterParameterGroupName"])?;
                Self::Parameters {
                    group: text(pairs, "DBClusterParameterGroupName")?,
                }
            }
            "DescribeDBSubnetGroups" => {
                let pairs = members(request, &["DBSubnetGroupName"])?;
                Self::SubnetGroups {
                    name: text(pairs, "DBSubnetGroupName")?,
                }
            }
            "ListTagsForResource" => {
                let pairs = members(request, &["ResourceName"])?;
                Self::Tags {
                    resource: text(pairs, "ResourceName")?,
                }
            }
            "GetMetricStatistics" => {
                let pairs = members(
                    request,
                    &[
                        "Namespace",
                        "MetricName",
                        "Dimensions",
                        "StartTime",
                        "EndTime",
                        "Period",
                        "Statistics",
                    ],
                )?;
                Self::Metric {
                    namespace: text(pairs, "Namespace")?,
                    metric: text(pairs, "MetricName")?,
                    dimensions: pairs_of(pairs, "Dimensions", "Name", "Value", |value| {
                        value.as_str().map(str::to_string).ok_or_else(unsupported)
                    })?,
                    start: time(pairs, "StartTime")?,
                    end: time(pairs, "EndTime")?,
                    period: int(pairs, "Period")?,
                    statistics: texts(pairs, "Statistics")?,
                }
            }
            "DescribeAlarms" => {
                let pairs = members(request, &["AlarmNamePrefix"])?;
                Self::Alarms {
                    prefix: text(pairs, "AlarmNamePrefix")?,
                }
            }
            "DescribeSecurityGroups" => {
                let pairs = members(request, &["GroupIds"])?;
                Self::Groups {
                    ids: texts(pairs, "GroupIds")?,
                }
            }
            "ListSecrets" => {
                let pairs = members(request, &["NextToken"])?;
                Self::Secrets {
                    token: text(pairs, "NextToken")?,
                }
            }
            _ => return Err(unsupported()),
        })
    }
}

/// Encode, send through the bridge and map one prepared request.
async fn send(prepared: &Prepared, region: &str, http: SharedHttpClient) -> Result<Json, AwsError> {
    match prepared {
        Prepared::Clusters { id } => {
            let output = sdk_client!(aws_sdk_docdb, region, http)
                .describe_db_clusters()
                .set_db_cluster_identifier(id.clone())
                .send()
                .await
                .map_err(|error| classify(&error))?;
            Ok(fields::clusters(&output))
        }
        Prepared::Instances { filters } => {
            let filters = filters.as_ref().map(|filters| {
                filters
                    .iter()
                    .map(|(name, values)| {
                        aws_sdk_docdb::types::Filter::builder()
                            .name(name)
                            .set_values(Some(values.clone()))
                            .build()
                    })
                    .collect()
            });
            let output = sdk_client!(aws_sdk_docdb, region, http)
                .describe_db_instances()
                .set_filters(filters)
                .send()
                .await
                .map_err(|error| classify(&error))?;
            Ok(fields::instances(&output))
        }
        Prepared::Events {
            source_identifier,
            source_type,
            start,
            end,
            marker,
        } => {
            let output = sdk_client!(aws_sdk_docdb, region, http)
                .describe_events()
                .set_source_identifier(source_identifier.clone())
                .set_source_type(
                    source_type
                        .as_deref()
                        .map(aws_sdk_docdb::types::SourceType::from),
                )
                .set_start_time(*start)
                .set_end_time(*end)
                .set_marker(marker.clone())
                .send()
                .await
                .map_err(|error| classify(&error))?;
            Ok(fields::events(&output))
        }
        Prepared::Parameters { group } => {
            let output = sdk_client!(aws_sdk_docdb, region, http)
                .describe_db_cluster_parameters()
                .set_db_cluster_parameter_group_name(group.clone())
                .send()
                .await
                .map_err(|error| classify(&error))?;
            Ok(fields::parameters(&output))
        }
        Prepared::SubnetGroups { name } => {
            let output = sdk_client!(aws_sdk_docdb, region, http)
                .describe_db_subnet_groups()
                .set_db_subnet_group_name(name.clone())
                .send()
                .await
                .map_err(|error| classify(&error))?;
            Ok(fields::subnet_groups(&output))
        }
        Prepared::Tags { resource } => {
            let output = sdk_client!(aws_sdk_docdb, region, http)
                .list_tags_for_resource()
                .set_resource_name(resource.clone())
                .send()
                .await
                .map_err(|error| classify(&error))?;
            Ok(fields::tags(&output))
        }
        Prepared::Metric {
            namespace,
            metric,
            dimensions,
            start,
            end,
            period,
            statistics,
        } => {
            let dimensions = dimensions.as_ref().map(|dimensions| {
                dimensions
                    .iter()
                    .map(|(name, value)| {
                        aws_sdk_cloudwatch::types::Dimension::builder()
                            .name(name)
                            .value(value)
                            .build()
                    })
                    .collect()
            });
            let statistics = statistics.as_ref().map(|statistics| {
                statistics
                    .iter()
                    .map(|statistic| aws_sdk_cloudwatch::types::Statistic::from(statistic.as_str()))
                    .collect()
            });
            let output = sdk_client!(aws_sdk_cloudwatch, region, http)
                .get_metric_statistics()
                .set_namespace(namespace.clone())
                .set_metric_name(metric.clone())
                .set_dimensions(dimensions)
                .set_start_time(*start)
                .set_end_time(*end)
                .set_period(*period)
                .set_statistics(statistics)
                .send()
                .await
                .map_err(|error| classify(&error))?;
            fields::datapoints(&output)
        }
        Prepared::Alarms { prefix } => {
            let output = sdk_client!(aws_sdk_cloudwatch, region, http)
                .describe_alarms()
                .set_alarm_name_prefix(prefix.clone())
                .send()
                .await
                .map_err(|error| classify(&error))?;
            Ok(fields::alarms(&output))
        }
        Prepared::Groups { ids } => {
            let output = sdk_client!(aws_sdk_ec2, region, http)
                .describe_security_groups()
                .set_group_ids(ids.clone())
                .send()
                .await
                .map_err(|error| classify(&error))?;
            Ok(fields::security_groups(&output))
        }
        Prepared::Secrets { token } => {
            let output = sdk_client!(aws_sdk_secretsmanager, region, http)
                .list_secrets()
                .set_next_token(token.clone())
                .send()
                .await
                .map_err(|error| classify(&error))?;
            Ok(fields::secrets(&output))
        }
    }
}

/// The SDK-to-seam field map (`docs/migration/docgraph.md`, "S2 field map"):
/// only members the checks read, absent values omitted, list order kept.
mod fields {
    use toolkit_aws::AwsError;
    use toolkit_core::Json;

    /// A seam object under construction.
    #[derive(Default)]
    struct Object(Vec<(String, Json)>);

    impl Object {
        fn put(mut self, key: &str, value: Option<Json>) -> Self {
            if let Some(value) = value {
                self.0.push((key.to_string(), value));
            }
            self
        }

        fn text(self, key: &str, value: Option<&String>) -> Self {
            self.put(key, value.map(|value| Json::string(value.clone())))
        }

        fn int(self, key: &str, value: Option<i32>) -> Self {
            self.put(key, value.map(|value| Json::Int(i64::from(value))))
        }

        fn boolean(self, key: &str, value: Option<bool>) -> Self {
            self.put(key, value.map(Json::Bool))
        }

        fn list<T>(self, key: &str, items: Option<&Vec<T>>, map: impl Fn(&T) -> Json) -> Self {
            self.put(
                key,
                items.map(|items| Json::Array(items.iter().map(map).collect())),
            )
        }

        fn strings(self, key: &str, items: Option<&Vec<String>>) -> Self {
            self.list(key, items, |item| Json::string(item.clone()))
        }

        fn object(self, key: &str, value: Option<Json>) -> Self {
            self.put(key, value)
        }

        fn build(self) -> Json {
            Json::Object(self.0)
        }
    }

    fn object() -> Object {
        Object::default()
    }

    pub(super) fn clusters(
        output: &aws_sdk_docdb::operation::describe_db_clusters::DescribeDbClustersOutput,
    ) -> Json {
        object()
            .list("DBClusters", output.db_clusters.as_ref(), |cluster| {
                object()
                    .int("BackupRetentionPeriod", cluster.backup_retention_period)
                    .boolean("DeletionProtection", cluster.deletion_protection)
                    .text("EngineVersion", cluster.engine_version.as_ref())
                    .text(
                        "DBClusterParameterGroup",
                        cluster.db_cluster_parameter_group.as_ref(),
                    )
                    .list(
                        "VpcSecurityGroups",
                        cluster.vpc_security_groups.as_ref(),
                        |group| {
                            object()
                                .text("VpcSecurityGroupId", group.vpc_security_group_id.as_ref())
                                .build()
                        },
                    )
                    .boolean("StorageEncrypted", cluster.storage_encrypted)
                    .strings(
                        "EnabledCloudwatchLogsExports",
                        cluster.enabled_cloudwatch_logs_exports.as_ref(),
                    )
                    .text("DBSubnetGroup", cluster.db_subnet_group.as_ref())
                    .text(
                        "PreferredMaintenanceWindow",
                        cluster.preferred_maintenance_window.as_ref(),
                    )
                    .text("DBClusterArn", cluster.db_cluster_arn.as_ref())
                    .text("StorageType", cluster.storage_type.as_ref())
                    .build()
            })
            .build()
    }

    pub(super) fn instances(
        output: &aws_sdk_docdb::operation::describe_db_instances::DescribeDbInstancesOutput,
    ) -> Json {
        object()
            .list("DBInstances", output.db_instances.as_ref(), |instance| {
                object()
                    .text(
                        "DBInstanceIdentifier",
                        instance.db_instance_identifier.as_ref(),
                    )
                    .text("DBInstanceClass", instance.db_instance_class.as_ref())
                    .text("AvailabilityZone", instance.availability_zone.as_ref())
                    .build()
            })
            .build()
    }

    pub(super) fn events(
        output: &aws_sdk_docdb::operation::describe_events::DescribeEventsOutput,
    ) -> Json {
        object()
            .list("Events", output.events.as_ref(), |event| {
                object()
                    .text("Message", event.message.as_ref())
                    .strings("EventCategories", event.event_categories.as_ref())
                    .build()
            })
            .text("Marker", output.marker.as_ref())
            .build()
    }

    pub(super) fn parameters(
        output: &aws_sdk_docdb::operation::describe_db_cluster_parameters::DescribeDbClusterParametersOutput,
    ) -> Json {
        object()
            .list("Parameters", output.parameters.as_ref(), |parameter| {
                object()
                    .text("ParameterName", parameter.parameter_name.as_ref())
                    .text("ParameterValue", parameter.parameter_value.as_ref())
                    .build()
            })
            .build()
    }

    pub(super) fn subnet_groups(
        output: &aws_sdk_docdb::operation::describe_db_subnet_groups::DescribeDbSubnetGroupsOutput,
    ) -> Json {
        object()
            .list(
                "DBSubnetGroups",
                output.db_subnet_groups.as_ref(),
                |group| {
                    object()
                        .list("Subnets", group.subnets.as_ref(), |subnet| {
                            object()
                                .object(
                                    "SubnetAvailabilityZone",
                                    subnet.subnet_availability_zone.as_ref().map(|zone| {
                                        object().text("Name", zone.name.as_ref()).build()
                                    }),
                                )
                                .build()
                        })
                        .build()
                },
            )
            .build()
    }

    pub(super) fn tags(
        output: &aws_sdk_docdb::operation::list_tags_for_resource::ListTagsForResourceOutput,
    ) -> Json {
        object()
            .list("TagList", output.tag_list.as_ref(), |tag| {
                object()
                    .text("Key", tag.key.as_ref())
                    .text("Value", tag.value.as_ref())
                    .build()
            })
            .build()
    }

    /// A non-finite statistic is not a number the checks can use.
    fn statistic(value: Option<f64>) -> Result<Option<Json>, AwsError> {
        match value {
            Some(value) if !value.is_finite() => Err(AwsError::invalid_response(
                "a metric datapoint holds a non-finite value",
            )),
            other => Ok(other.map(Json::Float)),
        }
    }

    pub(super) fn datapoints(
        output: &aws_sdk_cloudwatch::operation::get_metric_statistics::GetMetricStatisticsOutput,
    ) -> Result<Json, AwsError> {
        let points = match output.datapoints.as_ref() {
            None => None,
            Some(points) => Some(Json::Array(
                points
                    .iter()
                    .map(|point| {
                        Ok(object()
                            .put("SampleCount", statistic(point.sample_count)?)
                            .put("Average", statistic(point.average)?)
                            .put("Sum", statistic(point.sum)?)
                            .put("Minimum", statistic(point.minimum)?)
                            .put("Maximum", statistic(point.maximum)?)
                            .build())
                    })
                    .collect::<Result<Vec<_>, AwsError>>()?,
            )),
        };
        Ok(object().put("Datapoints", points).build())
    }

    pub(super) fn alarms(
        output: &aws_sdk_cloudwatch::operation::describe_alarms::DescribeAlarmsOutput,
    ) -> Json {
        object()
            .list("MetricAlarms", output.metric_alarms.as_ref(), |alarm| {
                object()
                    .text("AlarmName", alarm.alarm_name.as_ref())
                    .build()
            })
            .build()
    }

    pub(super) fn security_groups(
        output: &aws_sdk_ec2::operation::describe_security_groups::DescribeSecurityGroupsOutput,
    ) -> Json {
        object()
            .list("SecurityGroups", output.security_groups.as_ref(), |group| {
                object()
                    .list("IpPermissions", group.ip_permissions.as_ref(), |rule| {
                        object()
                            .list("IpRanges", rule.ip_ranges.as_ref(), |range| {
                                object().text("CidrIp", range.cidr_ip.as_ref()).build()
                            })
                            .list("Ipv6Ranges", rule.ipv6_ranges.as_ref(), |range| {
                                object().text("CidrIpv6", range.cidr_ipv6.as_ref()).build()
                            })
                            .build()
                    })
                    .build()
            })
            .build()
    }

    pub(super) fn secrets(
        output: &aws_sdk_secretsmanager::operation::list_secrets::ListSecretsOutput,
    ) -> Json {
        object()
            .list("SecretList", output.secret_list.as_ref(), |secret| {
                object()
                    .text("Name", secret.name.as_ref())
                    .text("Description", secret.description.as_ref())
                    .build()
            })
            .text("NextToken", output.next_token.as_ref())
            .build()
    }
}

/// The request the SDK generates for `request`, captured at the bridge and
/// never sent (T-DG-04e).
#[cfg(test)]
pub(crate) fn generated_request(
    op: &Operation,
    region: &str,
    request: &Json,
) -> Result<SdkHttpRequest, AwsError> {
    let prepared = Prepared::parse(op, request)?;
    let exchange = Exchange::default();
    let mut captured = None;
    let _ = drive(
        send(&prepared, region, exchange.http_client()),
        &exchange,
        |request| {
            captured = Some(request);
            Err(AwsError::connect("captured; not sent"))
        },
    )?;
    captured.ok_or_else(|| AwsError::invalid_config("request", "the SDK generated no request"))
}
