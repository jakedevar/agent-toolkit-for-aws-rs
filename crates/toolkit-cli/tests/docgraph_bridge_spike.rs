#![cfg(feature = "docgraph-live")]

//! Offline SDK orchestration experiment; it is not a live adapter.
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use aws_credential_types::provider::{future, ProvideCredentials};
use aws_smithy_runtime_api::box_error::BoxError;
use aws_smithy_runtime_api::client::auth::{
    AuthScheme, AuthSchemeEndpointConfig, AuthSchemeId, Sign,
};
use aws_smithy_runtime_api::client::connection::{CaptureSmithyConnection, ConnectionMetadata};
use aws_smithy_runtime_api::client::http::{
    http_client_fn, HttpConnector, HttpConnectorFuture, SharedHttpConnector,
};
use aws_smithy_runtime_api::client::identity::{Identity, SharedIdentityResolver};
use aws_smithy_runtime_api::client::orchestrator::{HttpRequest, HttpResponse};
use aws_smithy_runtime_api::client::result::ConnectorError;
use aws_smithy_runtime_api::client::runtime_components::{GetIdentityResolver, RuntimeComponents};
use aws_smithy_runtime_api::http::StatusCode;
use aws_smithy_types::body::SdkBody;
use aws_smithy_types::config_bag::ConfigBag;
use aws_smithy_types::retry::RetryConfig;
use aws_smithy_types::timeout::TimeoutConfig;
use toolkit_aws::sdk::{SdkRuntime, SdkTransport, SigV4Signer};
use toolkit_aws::testing::FakeProvider;
use toolkit_aws::transport::{
    Headers, HttpRequest as ToolkitRequest, Method, Signer, SigningScope, Timeouts, Transport,
    FRAMING_HEADERS,
};

type CapturedRequests = Vec<(Vec<String>, ToolkitRequest)>;

#[derive(Clone, Debug, Default)]
struct Spy(Arc<Mutex<CapturedRequests>>);

#[derive(Debug)]
struct ForbiddenProvider;

#[derive(Debug)]
struct SigningSpy {
    inner: aws_runtime::auth::sigv4::SigV4AuthScheme,
    calls: Arc<AtomicUsize>,
}

impl AuthScheme for SigningSpy {
    fn scheme_id(&self) -> AuthSchemeId {
        self.inner.scheme_id()
    }
    fn identity_resolver(
        &self,
        resolvers: &dyn GetIdentityResolver,
    ) -> Option<SharedIdentityResolver> {
        self.inner.identity_resolver(resolvers)
    }
    fn signer(&self) -> &dyn Sign {
        self
    }
}

impl Sign for SigningSpy {
    fn sign_http_request(
        &self,
        request: &mut HttpRequest,
        identity: &Identity,
        endpoint: AuthSchemeEndpointConfig<'_>,
        components: &RuntimeComponents,
        config: &ConfigBag,
    ) -> Result<(), BoxError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner
            .signer()
            .sign_http_request(request, identity, endpoint, components, config)
    }
}

impl ProvideCredentials for ForbiddenProvider {
    fn provide_credentials<'a>(&'a self) -> future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        panic!("SDK credential provider must never run")
    }
}

impl HttpConnector for Spy {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        let Some(buffered_body) = request.body().bytes() else {
            return HttpConnectorFuture::ready(Err(ConnectorError::user(
                std::io::Error::other("streaming request body is unsupported").into(),
            )));
        };
        let headers = request
            .headers()
            .iter()
            .map(|(name, _)| name.to_ascii_lowercase())
            .collect();
        let mut semantic_headers = Headers::new();
        for (name, value) in request.headers() {
            if !FRAMING_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
                semantic_headers.insert(name, value).unwrap();
            }
        }
        let body = buffered_body.to_vec();
        let mut signed = ToolkitRequest {
            method: match request.method() {
                "GET" => Method::Get,
                "POST" => Method::Post,
                other => panic!("unexpected method {other}"),
            },
            url: request.uri().to_string(),
            headers: semantic_headers,
            body: body.clone(),
            timeouts: Timeouts::default(),
        };
        let toolkit_signer = SigV4Signer::new(FakeProvider::with_keys(
            "fixture",
            "fixture-key",
            "fixture-secret",
        ));
        toolkit_signer
            .sign(
                &mut signed,
                &SigningScope {
                    region: "us-east-1".into(),
                    service: "fixture".into(),
                },
            )
            .unwrap();
        assert_eq!(signed.body, body);
        assert!(signed.headers.get("authorization").is_some());
        assert!(FRAMING_HEADERS
            .iter()
            .all(|name| signed.headers.get(name).is_none()));
        self.0.lock().unwrap().push((headers, signed));
        HttpConnectorFuture::ready(Ok(HttpResponse::new(
            StatusCode::try_from(503).unwrap(),
            SdkBody::empty(),
        )))
    }
}

type TransmittedRequests = Vec<(Vec<(String, String)>, Vec<u8>)>;

#[derive(Clone, Debug, Default)]
struct SentSpy(Arc<Mutex<TransmittedRequests>>);

impl HttpConnector for SentSpy {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        if let Some(capture) = request.extension::<CaptureSmithyConnection>() {
            capture.set_connection_retriever(|| {
                Some(
                    ConnectionMetadata::builder()
                        .proxied(false)
                        .remote_addr("8.8.8.8:443".parse().unwrap())
                        .poison_fn(|| {})
                        .build(),
                )
            });
        }
        let headers = request
            .headers()
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        let body = request.body().bytes().expect("buffered request").to_vec();
        self.0.lock().unwrap().push((headers, body));
        HttpConnectorFuture::ready(Ok(HttpResponse::new(
            StatusCode::try_from(503).unwrap(),
            SdkBody::empty(),
        )))
    }
}

#[test]
fn service_orchestrators_use_only_the_injected_connector() {
    let spy = Spy::default();
    let signer_calls = Arc::new(AtomicUsize::new(0));
    let connector = spy.clone();
    let http = http_client_fn(move |_, _| SharedHttpConnector::new(connector.clone()));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        for with_trap in [false, true] {
            macro_rules! config {
                ($service:ident) => {{
                    let mut builder = $service::Config::builder()
                        .behavior_version_latest()
                        .region($service::config::Region::new("us-east-1"))
                        .http_client(http.clone())
                        .allow_no_auth()
                        .auth_scheme_preference([AuthSchemeId::new("noAuth")])
                        .push_auth_scheme(SigningSpy {
                            inner: aws_runtime::auth::sigv4::SigV4AuthScheme::new(),
                            calls: signer_calls.clone(),
                        })
                        .retry_config(RetryConfig::disabled())
                        .timeout_config(TimeoutConfig::disabled());
                    if with_trap {
                        builder = builder.credentials_provider(ForbiddenProvider);
                    }
                    let config = builder.build();
                    assert!(config.identity_cache().is_none());
                    assert_eq!(config.retry_config().unwrap().max_attempts(), 1);
                    config
                }};
            }
            let docdb = aws_sdk_docdb::Client::from_conf(config!(aws_sdk_docdb));
            let _ = docdb.describe_db_clusters().send().await;
            let _ = docdb.describe_db_instances().send().await;
            let _ = docdb.describe_events().send().await;
            let _ = docdb
                .describe_db_cluster_parameters()
                .db_cluster_parameter_group_name("fixture")
                .send()
                .await;
            let _ = docdb.describe_db_subnet_groups().send().await;
            let _ = docdb
                .list_tags_for_resource()
                .resource_name("arn:aws:rds:us-east-1:123456789012:cluster:fixture")
                .send()
                .await;
            let cloudwatch = aws_sdk_cloudwatch::Client::from_conf(config!(aws_sdk_cloudwatch));
            let _ = cloudwatch.describe_alarms().send().await;
            let _ = cloudwatch
                .get_metric_statistics()
                .namespace("fixture")
                .metric_name("fixture")
                .start_time(aws_smithy_types::DateTime::from_secs(0))
                .end_time(aws_smithy_types::DateTime::from_secs(60))
                .period(60)
                .statistics(aws_sdk_cloudwatch::types::Statistic::Average)
                .send()
                .await;
            let _ = aws_sdk_ec2::Client::from_conf(config!(aws_sdk_ec2))
                .describe_security_groups()
                .send()
                .await;
            let _ = aws_sdk_secretsmanager::Client::from_conf(config!(aws_sdk_secretsmanager))
                .list_secrets()
                .send()
                .await;
        }
    });
    assert_eq!(
        signer_calls.load(Ordering::SeqCst),
        0,
        "SDK signer must not run"
    );
    runtime.block_on(signer_negative_control(signer_calls));
    runtime.block_on(refuse_streaming_body(&spy));
    let calls = spy.0.lock().unwrap();
    assert_eq!(
        calls.len(),
        20,
        "one injected connector call per allowed operation and configuration"
    );
    verify_transmission(&calls);
}

async fn refuse_streaming_body(spy: &Spy) {
    let body = aws_smithy_types::byte_stream::ByteStream::from_path(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/Cargo.toml"
    ))
    .await
    .unwrap()
    .into_inner();
    assert!(body.bytes().is_none());
    let result = spy.call(HttpRequest::new(body)).await;
    assert!(result.is_err(), "streaming bodies must be refused");
}

fn verify_transmission(calls: &CapturedRequests) {
    let sent = SentSpy::default();
    let transport = SdkTransport::with_connector(&SdkRuntime::new().unwrap(), sent.clone());
    for (headers, signed) in calls {
        assert!(!headers.iter().any(|h| h == "authorization"
            || h == "x-amz-date"
            || h == "x-amz-security-token"
            || h == "x-amz-content-sha256"));
        assert_eq!(transport.send(signed).unwrap().status, 503);
    }
    let transmitted = sent.0.lock().unwrap();
    assert_eq!(transmitted.len(), calls.len());
    for ((_, signed), (headers, body)) in calls.iter().zip(transmitted.iter()) {
        assert_eq!(&signed.body, body);
        for (name, value) in signed.headers.entries() {
            assert!(
                headers.iter().any(|(n, v)| n == name && v == value),
                "signed header missing from sent request"
            );
        }
    }
}

// Without the no-auth preference, synthetic credentials exercise the spy.
async fn signer_negative_control(signer_calls: Arc<AtomicUsize>) {
    let negative_connector = SentSpy::default();
    let negative_http =
        http_client_fn(move |_, _| SharedHttpConnector::new(negative_connector.clone()));
    let config = aws_sdk_docdb::Config::builder()
        .behavior_version_latest()
        .region(aws_sdk_docdb::config::Region::new("us-east-1"))
        .http_client(negative_http)
        .allow_no_auth()
        .push_auth_scheme(SigningSpy {
            inner: aws_runtime::auth::sigv4::SigV4AuthScheme::new(),
            calls: signer_calls.clone(),
        })
        .credentials_provider(aws_credential_types::Credentials::new(
            "fixture",
            "fixture-secret",
            None,
            None,
            "fixture",
        ))
        .retry_config(RetryConfig::disabled())
        .timeout_config(TimeoutConfig::disabled())
        .build();
    let _ = aws_sdk_docdb::Client::from_conf(config)
        .describe_db_clusters()
        .send()
        .await;
    assert_eq!(
        signer_calls.load(Ordering::SeqCst),
        1,
        "negative control must invoke SDK signer once"
    );
}
