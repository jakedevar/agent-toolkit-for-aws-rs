#![cfg(feature = "docgraph-live")]

//! Offline SDK orchestration experiment; it is not a live adapter.
use std::sync::{Arc, Mutex};

use aws_smithy_runtime_api::client::http::{
    http_client_fn, HttpConnector, HttpConnectorFuture, SharedHttpConnector,
};
use aws_smithy_runtime_api::client::orchestrator::{HttpRequest, HttpResponse};
use aws_smithy_runtime_api::http::StatusCode;
use aws_smithy_types::body::SdkBody;
use aws_smithy_types::retry::RetryConfig;
use aws_smithy_types::timeout::TimeoutConfig;
use toolkit_aws::sdk::SigV4Signer;
use toolkit_aws::testing::FakeProvider;
use toolkit_aws::transport::{
    Headers, HttpRequest as ToolkitRequest, Method, Signer, SigningScope, Timeouts, FRAMING_HEADERS,
};

#[derive(Clone, Debug, Default)]
struct Spy(Arc<Mutex<Vec<Vec<String>>>>);

impl HttpConnector for Spy {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        let headers = request
            .headers()
            .iter()
            .map(|(name, _)| name.to_ascii_lowercase())
            .collect();
        self.0.lock().unwrap().push(headers);
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
        macro_rules! config {
            ($service:ident) => {{
                let config = $service::Config::builder()
                    .behavior_version_latest()
                    .region($service::config::Region::new("us-east-1"))
                    .http_client(http.clone())
                    .allow_no_auth()
                    .retry_config(RetryConfig::disabled())
                    .timeout_config(TimeoutConfig::disabled())
                    .build();
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
    });
    let calls = spy.0.lock().unwrap();
    assert_eq!(calls.len(), 4, "one injected connector call per service");
    for headers in calls.iter() {
        assert!(!headers.iter().any(|h| h == "authorization"
            || h == "x-amz-date"
            || h == "x-amz-security-token"
            || h == "x-amz-content-sha256"));
    }
}
