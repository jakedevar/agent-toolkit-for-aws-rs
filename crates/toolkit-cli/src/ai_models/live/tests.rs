//! SDK wire contracts: synthetic responses and an injected transport only.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::*;
use crate::ai_models::{
    diagnostics::{collect_endpoint_diagnostics, TimeRange},
    hub::list_hub_contents,
};
use crate::cloudwatch::timefmt::UtcTime;
use toolkit_aws::{Headers, HttpRequest, HttpResponse};
use toolkit_core::parse_json;

fn j(text: &str) -> Json {
    canonical(parse_json(text).unwrap())
}
// JSON object member order is not part of the AWS wire protocol.
fn canonical(value: Json) -> Json {
    match value {
        Json::Object(members) => {
            let mut members: Vec<_> = members
                .into_iter()
                .map(|(k, v)| (k, canonical(v)))
                .collect();
            members.sort_by(|a, b| a.0.cmp(&b.0));
            Json::Object(members)
        }
        Json::Array(values) => Json::Array(values.into_iter().map(canonical).collect()),
        other => other,
    }
}
trait Array {
    fn as_array(&self) -> Option<&[Json]>;
}
impl Array for Json {
    fn as_array(&self) -> Option<&[Json]> {
        if let Json::Array(values) = self {
            Some(values)
        } else {
            None
        }
    }
}

type Requests = Arc<Mutex<Vec<HttpRequest>>>;
fn fixture(responses: Vec<(u16, &'static str)>) -> (LiveClient, Requests) {
    let requests = Requests::default();
    let captured = requests.clone();
    let responses = Mutex::new(VecDeque::from(responses));
    let client = LiveClient::with_sender(move |request| {
        let cbor = request.url.contains("monitoring.");
        captured.lock().unwrap().push(request);
        let (status, body) = responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("no unexpected requests/retries");
        let mut headers = Headers::new();
        let body = if cbor {
            headers.insert("content-type", "application/cbor").unwrap();
            headers.insert("smithy-protocol", "rpc-v2-cbor").unwrap();
            let mut encoder = minicbor::Encoder::new(Vec::new());
            encode_cbor(&mut encoder, &j(body), false);
            encoder.into_writer()
        } else {
            body.as_bytes().to_vec()
        };
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    });
    (client, requests)
}

fn wire(request: &HttpRequest, service: &str, target: &str) -> Json {
    let path = if service == "monitoring" {
        "service/GraniteServiceVersion20100801/operation/GetMetricData"
    } else {
        ""
    };
    let prefix = if service == "sagemaker" {
        "api.sagemaker"
    } else {
        service
    };
    assert_eq!(
        request.url,
        format!("https://{prefix}.us-west-2.amazonaws.com/{path}")
    );
    assert_eq!(request.method, toolkit_aws::Method::Post);
    if service == "monitoring" {
        assert_eq!(request.headers.get("smithy-protocol"), Some("rpc-v2-cbor"));
        assert_eq!(
            request.headers.get("content-type"),
            Some("application/cbor")
        );
    } else {
        assert_eq!(request.headers.get("x-amz-target"), Some(target));
    }
    assert!(
        request.headers.get("authorization").is_none(),
        "SDK signer must be off"
    );
    if service == "monitoring" {
        canonical(decode_cbor(&mut minicbor::Decoder::new(&request.body)))
    } else {
        j(std::str::from_utf8(&request.body).unwrap())
    }
}

// A fixture codec independent of the SDK's encoder/decoder. Only test data
// uses it. Timestamps are CBOR tag 1, seconds (Logs remains JSON millis).
fn encode_cbor(e: &mut minicbor::Encoder<Vec<u8>>, value: &Json, timestamp: bool) {
    if timestamp && !matches!(value, Json::Array(_)) {
        e.tag(minicbor::data::Tag::new(1)).unwrap();
    }
    match value {
        Json::Object(members) => {
            e.map(members.len() as u64).unwrap();
            for (key, value) in members {
                e.str(key).unwrap();
                encode_cbor(e, value, key == "Timestamps");
            }
        }
        Json::Array(values) => {
            e.array(values.len() as u64).unwrap();
            for value in values {
                encode_cbor(e, value, timestamp);
            }
        }
        Json::String(value) => {
            e.str(value).unwrap();
        }
        Json::Int(value) => {
            e.i64(*value).unwrap();
        }
        Json::Float(value) => {
            e.f64(*value).unwrap();
        }
        Json::Bool(value) => {
            e.bool(*value).unwrap();
        }
        Json::Null => {
            e.null().unwrap();
        }
    }
}
fn decode_cbor(d: &mut minicbor::Decoder<'_>) -> Json {
    use minicbor::data::Type;
    match d.datatype().unwrap() {
        Type::Map | Type::MapIndef => {
            let len = d.map().unwrap();
            let mut members = Vec::new();
            while len.map_or_else(
                || d.datatype().unwrap() != Type::Break,
                |n| members.len() < usize::try_from(n).unwrap(),
            ) {
                let key = d.str().unwrap().to_string();
                members.push((key, decode_cbor(d)));
            }
            if len.is_none() {
                d.skip().unwrap();
            }
            Json::Object(members)
        }
        Type::Array | Type::ArrayIndef => {
            let len = d.array().unwrap();
            let mut values = Vec::new();
            while len.map_or_else(
                || d.datatype().unwrap() != Type::Break,
                |n| values.len() < usize::try_from(n).unwrap(),
            ) {
                values.push(decode_cbor(d));
            }
            if len.is_none() {
                d.skip().unwrap();
            }
            Json::Array(values)
        }
        Type::Tag => {
            d.tag().unwrap();
            decode_cbor(d)
        }
        Type::F16 | Type::F32 | Type::F64 => Json::Float(d.f64().unwrap()),
        Type::String => Json::string(d.str().unwrap()),
        Type::Bool => Json::Bool(d.bool().unwrap()),
        Type::Null => {
            d.null().unwrap();
            Json::Null
        }
        _ => Json::Int(d.i64().unwrap()),
    }
}

#[test]
fn list_hub_contents_maps_and_paginates() {
    let (mut client, requests) = fixture(vec![
        (
            200,
            r#"{"HubContentSummaries":[{"HubContentName":"a","HubContentSearchKeywords":["@task:chat"],"OriginalCreationTime":1704164645.125}],"NextToken":"page2"}"#,
        ),
        (200, r#"{"HubContentSummaries":[{"HubContentName":"b"}]}"#),
    ]);
    let items = list_hub_contents(&mut client, "h1").unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0].get("OriginalCreationTime"),
        Some(&j("1704164645.125"))
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        wire(&requests[0], "sagemaker", "SageMaker.ListHubContents"),
        j(r#"{"HubName":"h1","HubContentType":"Model","MaxResults":100}"#)
    );
    assert_eq!(
        wire(&requests[1], "sagemaker", "SageMaker.ListHubContents"),
        j(r#"{"HubName":"h1","HubContentType":"Model","MaxResults":100,"NextToken":"page2"}"#)
    );
}

#[test]
fn describe_hub_content_maps_document_and_keywords() {
    let (mut client, requests) = fixture(vec![(
        200,
        r#"{"HubContentName":"m1","HubContentSearchKeywords":["@recipe:finetuning_sft_x"],"HubContentDocument":"{\"ModelSize\":\"7B\"}"}"#,
    )]);
    let request = j(r#"{"HubName":"h1","HubContentType":"Model","HubContentName":"m1"}"#);
    let response = client
        .call(Operation::DescribeHubContent, &request)
        .unwrap();
    assert_eq!(
        response.get("HubContentDocument"),
        Some(&Json::string(r#"{"ModelSize":"7B"}"#))
    );
    assert_eq!(
        response.get("HubContentSearchKeywords"),
        Some(&j(r#"["@recipe:finetuning_sft_x"]"#))
    );
    assert_eq!(
        wire(
            &requests.lock().unwrap()[0],
            "sagemaker",
            "SageMaker.DescribeHubContent"
        ),
        request
    );
}

#[test]
fn search_maps_nested_filters_and_pages() {
    let (mut client, requests) = fixture(vec![
        (
            200,
            r#"{"Results":[{"HubContent":{"HubContentName":"m1","HubContentSearchKeywords":["@task:chat"]}}],"NextToken":"next"}"#,
        ),
        (200, r#"{"Results":[]}"#),
    ]);
    client.check_search_support().unwrap();
    let expression = j(
        r#"{"Operator":"And","Filters":[{"Name":"HubName","Operator":"Equals","Value":"SageMakerPublicHub"}],"SubExpressions":[{"Operator":"Or","Filters":[{"Name":"HubContentSearchKeywords","Operator":"In","Value":"@task:chat,@task:Chat"}]}]}"#,
    );
    for token in [None, Some("next")] {
        let request = crate::ai_models::search::search_request(&expression, token);
        let response = client.call(Operation::Search, &request).unwrap();
        if token.is_none() {
            assert_eq!(response.get("NextToken"), Some(&Json::string("next")));
            assert_eq!(
                response.get("Results").unwrap().as_array().unwrap()[0]
                    .get("HubContent")
                    .unwrap()
                    .get("HubContentName"),
                Some(&Json::string("m1"))
            );
        }
        assert_eq!(
            wire(
                requests.lock().unwrap().last().unwrap(),
                "sagemaker",
                "SageMaker.Search"
            ),
            canonical(request)
        );
    }
}

#[test]
fn diagnostics_maps_endpoint_metrics_and_logs_without_extra_pages() {
    let (mut client, requests) = fixture(vec![
        (
            200,
            r#"{"EndpointStatus":"InService","CreationTime":1704164645.125,"LastModifiedTime":1704164646,"ProductionVariants":[{"VariantName":"Blue","CurrentInstanceCount":2,"CurrentWeight":1.0}]}"#,
        ),
        (
            200,
            r#"{"MetricDataResults":[{"Id":"invocations","Values":[3.5,2.0],"StatusCode":"Complete","Timestamps":[1704164645,1704164646]}],"NextToken":"metrics-next"}"#,
        ),
        (
            200,
            r#"{"events":[{"message":"hello","timestamp":1704164645125}],"nextToken":"logs-next"}"#,
        ),
    ]);
    let now = UtcTime::from_epoch_micros(1_704_164_646_000_000).unwrap();
    let response =
        collect_endpoint_diagnostics(&mut client, "my-endpoint", TimeRange::Last(60), now).unwrap();
    assert_eq!(
        response.get("metrics").unwrap().get("invocations"),
        Some(&j("[3.5,2.0]"))
    );
    assert_eq!(
        response.get("recent_logs").unwrap().as_array().unwrap()[0],
        j(r#"{"message":"hello","timestamp":"2024-01-02T03:04:05.125000+00:00"}"#)
    );
    assert_eq!(
        response
            .get("endpoint_status")
            .unwrap()
            .get("creation_time"),
        Some(&Json::string("2024-01-02 03:04:05.125000+00:00"))
    );
    let requests = requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        3,
        "diagnostics intentionally reads one page per operation"
    );
    assert_eq!(
        wire(&requests[0], "sagemaker", "SageMaker.DescribeEndpoint"),
        j(r#"{"EndpointName":"my-endpoint"}"#)
    );
    let metrics = wire(
        &requests[1],
        "monitoring",
        "GraniteServiceVersion20100801.GetMetricData",
    );
    assert_eq!(metrics.get("StartTime"), Some(&j("1704161046.0")));
    assert_eq!(metrics.get("EndTime"), Some(&j("1704164646.0")));
    let queries = metrics
        .get("MetricDataQueries")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(queries.len(), 11);
    assert_eq!(
        queries[0],
        j(
            r#"{"Id":"invocations","MetricStat":{"Metric":{"Namespace":"AWS/SageMaker","MetricName":"Invocations","Dimensions":[{"Name":"EndpointName","Value":"my-endpoint"},{"Name":"VariantName","Value":"Blue"}]},"Period":60,"Stat":"Sum"}}"#
        )
    );
    assert_eq!(
        queries[5].get("MetricStat").unwrap().get("Stat"),
        Some(&Json::string("p99"))
    );
    assert_eq!(
        wire(&requests[2], "logs", "Logs_20140328.FilterLogEvents"),
        j(
            r#"{"logGroupName":"/aws/sagemaker/Endpoints/my-endpoint","startTime":1704163746000,"endTime":1704164646000,"limit":100}"#
        )
    );
}

#[test]
fn service_errors_keep_code_message_and_status() {
    for operation in Operation::ALL {
        let (mut client, _) = fixture(vec![(
            400,
            r#"{"__type":"AccessDeniedException","message":"denied token=fixture-secret"}"#,
        )]);
        let request = match operation {
            Operation::ListHubContents => {
                j(r#"{"HubName":"h","HubContentType":"Model","MaxResults":100}"#)
            }
            Operation::DescribeHubContent => {
                j(r#"{"HubName":"h","HubContentType":"Model","HubContentName":"m"}"#)
            }
            Operation::Search => j(
                r#"{"Resource":"HubContent","MaxResults":100,"SortBy":"HubContentName","SortOrder":"Ascending","SearchExpression":{"Filters":[{"Name":"HubName","Operator":"Equals","Value":"h"}]}}"#,
            ),
            Operation::DescribeEndpoint => j(r#"{"EndpointName":"e"}"#),
            Operation::GetMetricData => j(
                r#"{"StartTime":"2024-01-02T02:04:06+00:00","EndTime":"2024-01-02T03:04:06+00:00","MetricDataQueries":[{"Id":"m","MetricStat":{"Metric":{"Namespace":"n","MetricName":"m","Dimensions":[]},"Period":60,"Stat":"Sum"}}]}"#,
            ),
            Operation::FilterLogEvents => j(
                r#"{"logGroupName":"/aws/sagemaker/Endpoints/e","startTime":1,"endTime":2,"limit":100}"#,
            ),
        };
        let AwsError::Service(error) = client.call(operation, &request).unwrap_err() else {
            panic!("expected service error for {operation:?}")
        };
        assert_eq!(error.code(), Some("AccessDeniedException"));
        assert_eq!(error.status(), 400);
        assert!(error.message().contains("denied"));
        assert!(!error.message().contains("fixture-secret"));
    }
}

#[test]
fn repeated_page_token_remains_a_pagination_error() {
    let (mut client, requests) = fixture(vec![
        (200, r#"{"NextToken":"repeated"}"#),
        (200, r#"{"NextToken":"repeated"}"#),
    ]);
    assert!(matches!(
        list_hub_contents(&mut client, "h"),
        Err(AwsError::Pagination { .. })
    ));
    assert_eq!(requests.lock().unwrap().len(), 2);
}

#[test]
fn seam_selection_is_lazy_and_recorded_always_wins() {
    let client = LiveClient::new(Some("eu-west-1"));
    assert!(client.bridges.is_empty());
    assert_eq!(client.region.as_deref(), Some("eu-west-1"));
    let (live, requests) = fixture(vec![(200, r#"{"HubContentSummaries":[]}"#)]);
    let mut selected = crate::ai_models::select_client(None, || Box::new(live)).unwrap();
    list_hub_contents(selected.as_mut(), "h").unwrap();
    assert_eq!(requests.lock().unwrap().len(), 1);

    let path = std::env::temp_dir().join(format!("ai-models-seam-{}.json", std::process::id()));
    std::fs::write(
        &path,
        r#"{"calls":{"ListHubContents":[{"response":{"HubContentSummaries":[]}}]}}"#,
    )
    .unwrap();
    let mut selected = crate::ai_models::select_client(Some(path.to_str().unwrap()), || {
        panic!("recorded must not initialize live")
    })
    .unwrap();
    assert!(list_hub_contents(selected.as_mut(), "h")
        .unwrap()
        .is_empty());
    std::fs::remove_file(&path).unwrap();
    assert!(
        crate::ai_models::select_client(Some(path.to_str().unwrap()), || panic!(
            "bad recorded path must not fall through to live"
        ))
        .is_err()
    );
}

#[test]
fn transport_and_credential_errors_stay_typed_and_do_not_become_service_errors() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for error in [
        AwsError::NoCredentials,
        AwsError::connect("fixture"),
        AwsError::timeout(toolkit_aws::error::TimeoutPhase::Deadline, "fixture"),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let expected = error.clone();
        let mut client = LiveClient::with_sender(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Err(error.clone())
        });
        assert_eq!(
            client
                .call(Operation::DescribeEndpoint, &j(r#"{"EndpointName":"e"}"#))
                .unwrap_err(),
            expected
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn malformed_sdk_response_fails_closed_without_echoing_payload() {
    let (mut client, _) = fixture(vec![(200, "malformed fixture-secret")]);
    let error = client
        .call(Operation::DescribeEndpoint, &j(r#"{"EndpointName":"e"}"#))
        .unwrap_err();
    assert!(
        matches!(error, AwsError::InvalidResponse { .. }),
        "{error:?}"
    );
    assert!(!error.to_string().contains("fixture-secret"));
}

#[test]
fn toolkit_retry_policy_is_the_only_retry_owner() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let retry = toolkit_aws::RetryPolicy::new(
        2,
        toolkit_aws::RetryMode::Standard,
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .unwrap();
    let config = toolkit_aws::ClientConfig::resolve(
        "sagemaker",
        &toolkit_aws::ClientSettings {
            region: Some("us-west-2"),
            retry,
            ..toolkit_aws::ClientSettings::default()
        },
        &toolkit_aws::EndpointPolicy::aws_public(),
        &toolkit_aws::MapEnv::new(),
        &toolkit_aws::testing::FixedResolver::new(),
    )
    .unwrap();
    let mut bridge = ServiceBridge::with_sender(config, move |_, _| {
        let attempt = counter.fetch_add(1, Ordering::SeqCst);
        Ok(HttpResponse {
            status: if attempt < 2 { 503 } else { 200 },
            headers: Headers::new(),
            body: if attempt < 2 {
                br#"{"__type":"ServiceUnavailable","message":"busy"}"#.to_vec()
            } else {
                b"{}".to_vec()
            },
        })
    })
    .unwrap();
    let sdk = sdk_client!(aws_sdk_sagemaker, bridge);
    let call = sdk.describe_endpoint().endpoint_name("e");
    bridge.run(|| call.clone().send()).unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "two toolkit retries, no SDK retries"
    );
}

#[test]
fn search_pagination_runs_through_the_existing_collector() {
    let (mut client, requests) = fixture(vec![
        (
            200,
            r#"{"Results":[{"HubContent":{"HubContentName":"a"}}],"NextToken":"t1"}"#,
        ),
        (
            200,
            r#"{"Results":[{"HubContent":{"HubContentName":"b"}},{"HubContent":{"HubContentName":"a"}}]}"#,
        ),
    ]);
    let constraints = Vec::new();
    let models = crate::ai_models::search::search_deployable_models(
        "SageMakerPublicHub",
        &constraints,
        &[],
        &mut client,
    )
    .unwrap();
    assert_eq!(
        models.len(),
        2,
        "existing name de-duplication survives the live binding"
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        wire(&requests[1], "sagemaker", "SageMaker.Search").get("NextToken"),
        Some(&Json::string("t1"))
    );
}

#[test]
fn metric_dates_preserve_offsets_and_microseconds() {
    let utc = date(&j(r#"{"Time":"2024-01-02T03:04:05.123456+00:00"}"#), "Time").unwrap();
    let offset = date(&j(r#"{"Time":"2024-01-02T04:04:05.123456+01:00"}"#), "Time").unwrap();
    assert_eq!(utc, offset);
    assert_eq!(utc.secs(), 1_704_164_645);
    assert_eq!(utc.subsec_nanos(), 123_456_000);
}
