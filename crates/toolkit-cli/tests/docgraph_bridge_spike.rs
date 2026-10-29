#![cfg(feature = "docgraph-live")]

//! Offline SDK orchestration experiment; it is not a live adapter.
use std::sync::{Arc, Mutex};

use aws_credential_types::provider::{future, ProvideCredentials};
use aws_smithy_runtime_api::client::auth::AuthSchemeId;
use aws_smithy_runtime_api::client::connection::{CaptureSmithyConnection, ConnectionMetadata};
use aws_smithy_runtime_api::client::http::{
    http_client_fn, HttpConnector, HttpConnectorFuture, SharedHttpConnector,
};
use aws_smithy_runtime_api::client::orchestrator::{HttpRequest, HttpResponse};
use aws_smithy_runtime_api::http::StatusCode;
use aws_smithy_types::body::SdkBody;
use aws_smithy_types::retry::RetryConfig;
use aws_smithy_types::timeout::TimeoutConfig;
use toolkit_aws::sdk::{SdkRuntime, SdkTransport, SigV4Signer};
use toolkit_aws::testing::FakeProvider;
use toolkit_aws::transport::{
    Headers, HttpRequest as ToolkitRequest, Method, Signer, SigningScope, Timeouts, Transport,
    FRAMING_HEADERS,
};

#[derive(Clone, Debug, Default)]
struct Spy(Arc<Mutex<Vec<(Vec<String>, ToolkitRequest)>>>);

#[derive(Debug)]
struct ForbiddenProvider;

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
        let headers = request
            .headers()
            .iter()
            .map(|(name, _)| name.to_ascii_lowercase())
            .collect();
        let mut semantic_headers = Headers::new();
        for (name, value) in request.headers().iter() {
            if !FRAMING_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
                semantic_headers.insert(name, value).unwrap();
            }
        }
        let body = request
            .body()
            .bytes()
            .expect("only buffered bodies are supported")
            .to_vec();
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
        let signer = SigV4Signer::new(FakeProvider::with_keys(
            "fixture",
            "fixture-key",
            "fixture-secret",
        ));
        signer
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

#[derive(Clone, Debug, Default)]
struct SentSpy(Arc<Mutex<Vec<(Vec<(String, String)>, Vec<u8>)>>>);

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
            let _ = aws_sdk_docdb::Client::from_conf(config!(aws_sdk_docdb))
                .describe_db_clusters()
                .send()
                .await;
            let _ = aws_sdk_cloudwatch::Client::from_conf(config!(aws_sdk_cloudwatch))
                .describe_alarms()
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
    let calls = spy.0.lock().unwrap();
    assert_eq!(
        calls.len(),
        8,
        "one injected connector call per service and configuration"
    );
    let sent = SentSpy::default();
    let transport = SdkTransport::with_connector(&SdkRuntime::new().unwrap(), sent.clone());
    for (headers, signed) in calls.iter() {
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
