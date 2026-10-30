//! Generated SDKs serialize only into a capture connector. They have no
//! credentials, signing or retry owner and never receive a network connector.
//! After encoding returns, toolkit signs the captured final bytes and sends
//! them once outside the SDK runtime. Response bodies stay bounded/streamed.
use super::{admit, graph_id, object, only, text, DatabaseEndpoint, Duration, Instant, Json, Route, INVALID, UNKNOWN};
use aws_sdk_dynamodb::types::AttributeValue;
use aws_smithy_runtime_api::client::auth::AuthSchemeId;
use aws_smithy_runtime_api::client::http::{
    http_client_fn, HttpConnector, HttpConnectorFuture, SharedHttpConnector,
};
use aws_smithy_runtime_api::client::orchestrator::HttpRequest as ModeledRequest;
use aws_smithy_runtime_api::client::result::ConnectorError;
use aws_smithy_types::retry::RetryConfig;
use aws_smithy_types::timeout::TimeoutConfig;
use aws_smithy_types::{Document, Number};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use toolkit_aws::stream::{StreamLimits, StreamingTransport};
use toolkit_aws::transport::{
    Headers, HttpRequest, Method, Signer, SigningScope, Timeouts, FRAMING_HEADERS,
};
use toolkit_core::{parse_json_with_limits, JsonLimits};

const MAX_BYTES: usize = 16 * 1024 * 1024;
const LIMITS: JsonLimits = JsonLimits {
    max_depth: 64,
    max_nodes: 1_000_000,
};

pub(super) fn document(value: &Json) -> Result<Document, String> {
    Ok(match value {
        Json::Null => Document::Null,
        Json::Bool(b) => Document::Bool(*b),
        Json::String(s) => Document::String(s.clone()),
        Json::Int(n) => Document::Number(if *n < 0 {
            Number::NegInt(*n)
        } else {
            Number::PosInt(n.unsigned_abs())
        }),
        Json::Float(n) if n.is_finite() => Document::Number(Number::Float(*n)),
        Json::Array(a) => Document::Array(a.iter().map(document).collect::<Result<_, _>>()?),
        Json::Object(o) => Document::Object(
            o.iter()
                .map(|(k, v)| Ok((k.clone(), document(v)?)))
                .collect::<Result<_, String>>()?,
        ),
        Json::Float(_) => return Err(INVALID.into()),
    })
}
fn attribute(value: &Json) -> Result<AttributeValue, String> {
    Ok(match value {
        Json::Null => AttributeValue::Null(true),
        Json::Bool(b) => AttributeValue::Bool(*b),
        Json::String(s) => AttributeValue::S(s.clone()),
        Json::Int(n) => AttributeValue::N(n.to_string()),
        Json::Array(a) => AttributeValue::L(a.iter().map(attribute).collect::<Result<_, _>>()?),
        Json::Object(_) => AttributeValue::M(attributes(value)?),
        // This seam has integer timestamps/TTL. Reject decimals instead of
        // introducing a lossy float/Decimal compatibility contract.
        Json::Float(_) => return Err(INVALID.into()),
    })
}
fn attributes(value: &Json) -> Result<HashMap<String, AttributeValue>, String> {
    object(value)?
        .iter()
        .map(|(k, v)| Ok((k.clone(), attribute(v)?)))
        .collect()
}
fn integer(request: &Json, key: &str, min: i64, max: i64) -> Result<i32, String> {
    match request.get(key) {
        Some(Json::Int(n)) if (min..=max).contains(n) => {
            i32::try_from(*n).map_err(|_| INVALID.into())
        }
        _ => Err(INVALID.into()),
    }
}
pub(super) fn validate_creation(r: &Json) -> Result<(), String> {
    let name = text(r, "graphName")?;
    if name.is_empty()
        || name.len() > 63
        || !name.as_bytes()[0].is_ascii_lowercase()
        || name.starts_with("g-")
        || name.ends_with('-')
        || name.contains("--")
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(INVALID.into());
    }
    if r.get("publicConnectivity") != Some(&Json::Bool(false))
        || r.get("deletionProtection") != Some(&Json::Bool(true))
    {
        return Err(INVALID.into());
    }
    integer(r, "provisionedMemory", 16, 24576)?;
    let vector = r.get("vectorSearchConfiguration").ok_or(INVALID)?;
    only(vector, &["dimension"])?;
    integer(vector, "dimension", 1, 65535)?;
    if r.get("replicaCount").is_some() {
        integer(r, "replicaCount", 0, 2)?;
    }
    let tags = r.get("tags").ok_or(INVALID)?;
    only(tags, &["created_by", "generation_model"])?;
    if text(tags, "created_by")? != "neptune-skill" || text(tags, "generation_model")?.len() > 256 {
        return Err(INVALID.into());
    }
    Ok(())
}
pub(super) fn validate_dynamo(r: &Json) -> Result<(), String> {
    let table = text(r, "table")?;
    if !(3..=255).contains(&table.len())
        || !table
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return Err(INVALID.into());
    }
    match text(r, "operation")? {
        "put_item" => {
            only(r, &["operation", "table", "Item"])?;
            let item = r.get("Item").ok_or(INVALID)?;
            only(item, &["session_id", "timestamp", "role", "content", "ttl"])?;
            for key in ["session_id", "role", "content"] {
                text(item, key)?;
            }
            if !matches!(item.get("timestamp"), Some(Json::Int(_)))
                || !matches!(item.get("ttl"), Some(Json::Int(_)))
            {
                return Err(INVALID.into());
            }
            attributes(item)?;
        }
        "query" => {
            only(
                r,
                &[
                    "operation",
                    "table",
                    "KeyConditionExpression",
                    "ExpressionAttributeValues",
                    "ScanIndexForward",
                    "Limit",
                ],
            )?;
            if text(r, "KeyConditionExpression")? != "session_id = :sid"
                || r.get("ScanIndexForward") != Some(&Json::Bool(false))
            {
                return Err(INVALID.into());
            }
            let values = r.get("ExpressionAttributeValues").ok_or(INVALID)?;
            only(values, &[":sid"])?;
            text(values, ":sid")?;
            integer(r, "Limit", 1, i64::from(i32::MAX))?;
        }
        _ => return Err(INVALID.into()),
    }
    Ok(())
}
#[derive(Clone, Default, Debug)]
struct Capture(Arc<Mutex<Option<HttpRequest>>>);
impl HttpConnector for Capture {
    fn call(&self, r: ModeledRequest) -> HttpConnectorFuture {
        let captured = (|| {
            let bytes = r.body().bytes().ok_or(INVALID)?;
            if bytes.len() > MAX_BYTES {
                return Err(INVALID);
            }
            let mut headers = Headers::new();
            for (name, value) in r.headers() {
                if !FRAMING_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
                    headers.insert(name, value).map_err(|_| INVALID)?;
                }
            }
            let method = match r.method() {
                "POST" => Method::Post,
                _ => return Err(INVALID),
            };
            Ok(HttpRequest {
                method,
                url: r.uri().to_string(),
                headers,
                body: bytes.to_vec(),
                timeouts: Timeouts::new(Duration::from_secs(10), Duration::from_secs(30))
                    .map_err(|_| INVALID)?,
            })
        })();
        if let Ok(request) = captured {
            if let Ok(mut slot) = self.0.lock() {
                if slot.is_none() {
                    *slot = Some(request);
                }
            }
        }
        // The SDK never sees a service response: encoding stops here. No
        // network connector can be selected, even if SDK auth/retries regress.
        HttpConnectorFuture::ready(Err(ConnectorError::user(
            std::io::Error::other("request captured").into(),
        )))
    }
}

pub(super) fn encode(
    route: Route,
    request: &Json,
    region: &str,
    policy: Option<&DatabaseEndpoint>,
    remaining: Duration,
) -> Result<HttpRequest, String> {
    if admit(request, policy)? != route {
        return Err(INVALID.into());
    }
    if policy.is_some_and(|p| p.region() != region) {
        return Err(INVALID.into());
    }
    let capture = Capture::default();
    let c = capture.clone();
    let http = http_client_fn(move |_, _| SharedHttpConnector::new(c.clone()));
    // Codec-only runtime, driven and dropped before toolkit credential/signing
    // or transport block_on starts; no nested SdkRuntime::block_on.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|_| INVALID)?;
    macro_rules! config {
        ($service:ident, $endpoint:expr) => {{
            let builder = $service::Config::builder()
                .behavior_version_latest()
                .region($service::config::Region::new(region.to_string()))
                .http_client(http)
                .allow_no_auth()
                .auth_scheme_preference([AuthSchemeId::new("noAuth")])
                .retry_config(RetryConfig::disabled())
                .timeout_config(TimeoutConfig::disabled())
                .endpoint_url($endpoint);
            #[cfg(test)]
            let builder = builder
                .credentials_provider(tests::ForbiddenProvider)
                .push_auth_scheme(tests::SigningTrap);
            let config = builder.build();
            #[cfg(test)]
            {
                assert_eq!(config.retry_config().unwrap().max_attempts(), 1);
                assert!(config.identity_cache().is_none());
            }
            config
        }};
    }
    runtime.block_on(async {
        match route {
            Route::Analytics => {
                let p = policy.ok_or(INVALID)?;
                let timeout = toolkit_aws::sdk::database::server_query_timeout(
                    remaining,
                    Duration::from_secs(1),
                    Duration::from_secs(55),
                )
                .and_then(|d| i32::try_from(d.as_millis()).ok())
                .filter(|ms| *ms > 0)
                .ok_or(INVALID)?;
                let client = aws_sdk_neptunegraph::Client::from_conf(config!(
                    aws_sdk_neptunegraph,
                    // The model prepends graphIdentifier as a host label.
                    format!("https://{}.neptune-graph.amazonaws.com:443", p.region())
                ));
                let parameters = request
                    .get("parameters")
                    .map(|v| {
                        object(v)?
                            .iter()
                            .map(|(k, v)| Ok((k.clone(), document(v)?)))
                            .collect::<Result<HashMap<_, _>, String>>()
                    })
                    .transpose()?;
                let _ = client
                    .execute_query()
                    .graph_identifier(text(request, "graphIdentifier")?)
                    .query_string(text(request, "queryString")?)
                    .language(aws_sdk_neptunegraph::types::QueryLanguage::OpenCypher)
                    .set_parameters(parameters)
                    .query_timeout_milliseconds(timeout)
                    .send()
                    .await;
            }
            Route::Create => {
                let tags = object(request.get("tags").ok_or(INVALID)?)?
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), v.as_str().ok_or(INVALID)?.to_string())))
                    .collect::<Result<HashMap<_, _>, String>>()?;
                let client = aws_sdk_neptunegraph::Client::from_conf(config!(
                    aws_sdk_neptunegraph,
                    format!("https://neptune-graph.{region}.amazonaws.com")
                ));
                let dimension = integer(
                    request.get("vectorSearchConfiguration").ok_or(INVALID)?,
                    "dimension",
                    1,
                    65535,
                )?;
                let vector = aws_sdk_neptunegraph::types::VectorSearchConfiguration::builder()
                    .dimension(dimension)
                    .build()
                    .map_err(|_| INVALID)?;
                let replica = request
                    .get("replicaCount")
                    .map(|_| integer(request, "replicaCount", 0, 2))
                    .transpose()?;
                let _ = client
                    .create_graph()
                    .graph_name(text(request, "graphName")?)
                    .provisioned_memory(integer(request, "provisionedMemory", 16, 24576)?)
                    .public_connectivity(false)
                    .deletion_protection(true)
                    .vector_search_configuration(vector)
                    .set_tags(Some(tags))
                    .set_replica_count(replica)
                    .send()
                    .await;
            }
            Route::Dynamo => {
                let client = aws_sdk_dynamodb::Client::from_conf(config!(
                    aws_sdk_dynamodb,
                    format!("https://dynamodb.{region}.amazonaws.com")
                ));
                if text(request, "operation")? == "put_item" {
                    let _ = client
                        .put_item()
                        .table_name(text(request, "table")?)
                        .set_item(Some(attributes(request.get("Item").ok_or(INVALID)?)?))
                        .send()
                        .await;
                } else {
                    let _ = client
                        .query()
                        .table_name(text(request, "table")?)
                        .key_condition_expression(text(request, "KeyConditionExpression")?)
                        .set_expression_attribute_values(Some(attributes(
                            request.get("ExpressionAttributeValues").ok_or(INVALID)?,
                        )?))
                        .scan_index_forward(false)
                        .limit(integer(request, "Limit", 1, i64::from(i32::MAX))?)
                        .send()
                        .await;
                }
            }
            Route::Gremlin => return Err(INVALID.into()),
        }
        Ok::<_, String>(())
    })?;
    let captured = capture
        .0
        .lock()
        .map_err(|_| INVALID)?
        .take()
        .ok_or(INVALID)?;
    let expected = match route {
        Route::Analytics => format!("https://{}:443/queries", policy.ok_or(INVALID)?.host()),
        Route::Create => format!("https://neptune-graph.{region}.amazonaws.com/graphs"),
        Route::Dynamo => format!("https://dynamodb.{region}.amazonaws.com/"),
        Route::Gremlin => return Err(INVALID.into()),
    };
    if captured.url != expected {return Err(INVALID.into());}
    Ok(captured)
}
fn decode_attribute(value: &Json) -> Result<Json, String> {
    let fields = object(value)?;
    if fields.len() != 1 {
        return Err(INVALID.into());
    }
    let (kind, v) = &fields[0];
    match (kind.as_str(), v) {
        ("S", Json::String(_)) | ("BOOL", Json::Bool(_)) => Ok(v.clone()),
        ("NULL", Json::Bool(true)) => Ok(Json::Null),
        ("N", Json::String(s)) => s.parse::<i64>().map(Json::Int).map_err(|_| INVALID.into()),
        ("L", Json::Array(items)) => Ok(Json::Array(
            items
                .iter()
                .map(decode_attribute)
                .collect::<Result<_, _>>()?,
        )),
        ("M", Json::Object(_)) => decode_attributes(v),
        _ => Err(INVALID.into()),
    }
}
fn decode_attributes(value: &Json) -> Result<Json, String> {
    Ok(Json::Object(
        object(value)?
            .iter()
            .map(|(k, v)| Ok((k.clone(), decode_attribute(v)?)))
            .collect::<Result<_, String>>()?,
    ))
}
pub(super) fn normalize(route: Route, bytes: &[u8]) -> Result<Json, String> {
    if bytes.len() > MAX_BYTES {
        return Err(INVALID.into());
    }
    let value = parse_json_with_limits(std::str::from_utf8(bytes).map_err(|_| INVALID)?, &LIMITS)
        .map_err(|_| INVALID)?;
    object(&value)?;
    match route {
        Route::Analytics => {
            // A missing results member is not a successful live query payload.
            match value.get("results") {
                Some(Json::Array(a)) if a.len() <= 100_000 => Ok(value),
                _ => Err(INVALID.into()),
            }
        }
        Route::Create => {
            let id = text(&value, "id")?;
            if !graph_id(id) {
                return Err(INVALID.into());
            }
            Ok(Json::object(vec![("id", Json::string(id))]))
        }
        Route::Dynamo => match value.get("Items") {
            Some(Json::Array(items)) if items.len() <= 100_000 => Ok(Json::object(vec![(
                "Items",
                Json::Array(
                    items
                        .iter()
                        .map(decode_attributes)
                        .collect::<Result<_, _>>()?,
                ),
            )])),
            None => Ok(Json::object(vec![])),
            _ => Err(INVALID.into()),
        },
        Route::Gremlin => Err(INVALID.into()),
    }
}
pub(super) fn exchange(
    transport: &dyn StreamingTransport,
    signer: &dyn Signer,
    route: Route,
    mut wire: HttpRequest,
    region: &str,
    deadline: Instant,
) -> Result<Json, String> {
    if Instant::now() >= deadline {
        return Err("the memory operation deadline elapsed before send".into());
    }
    let scope = SigningScope {
        region: region.into(),
        service: if route == Route::Dynamo {
            "dynamodb"
        } else {
            "neptune-graph"
        }
        .into(),
    };
    signer
        .sign(&mut wire, &scope)
        .map_err(|_| "the memory request could not be signed; nothing was sent")?;
    if wire.headers.get("authorization").is_none() || Instant::now() >= deadline {
        return Err(
            "the memory request requires valid credentials and remaining time; nothing was sent"
                .into(),
        );
    }
    if route == Route::Analytics {
        let body = toolkit_core::parse_json(std::str::from_utf8(&wire.body).map_err(|_| INVALID)?)
            .map_err(|_| INVALID)?;
        let Some(Json::Int(ms)) = body.get("queryTimeoutMilliseconds") else {
            return Err(INVALID.into());
        };
        if u64::try_from(*ms).ok().is_none_or(|ms| {
            Duration::from_millis(ms) >= deadline.saturating_duration_since(Instant::now())
        }) {
            return Err("the server query timeout would exceed the remaining client budget; nothing was sent".into());
        }
    }
    let limits = StreamLimits::new(MAX_BYTES as u64, 64 * 1024)
        .map_err(|_| INVALID)?
        .with_deadline(deadline);
    let mut stream = transport.open(&wire, &limits).map_err(|_| UNKNOWN)?;
    let expected = if route == Route::Create { 201 } else { 200 };
    if stream.status() != expected {
        return Err(UNKNOWN.into());
    }
    let mut bytes = Vec::new();
    loop {
        if Instant::now() >= deadline {
            return Err(UNKNOWN.into());
        }
        match stream.next_chunk().map_err(|_| UNKNOWN)? {
            Some(chunk) => {
                if bytes.len().saturating_add(chunk.len()) > MAX_BYTES {
                    return Err(UNKNOWN.into());
                }
                bytes.extend_from_slice(&chunk);
            }
            None => break,
        }
    }
    if Instant::now() >= deadline {
        return Err(UNKNOWN.into());
    }
    let result = normalize(route, &bytes).map_err(|_| UNKNOWN)?;
    if route == Route::Dynamo
        && wire.headers.get("x-amz-target") == Some("DynamoDB_20120810.Query")
        && result.get("Items").is_none()
    {
        return Err(UNKNOWN.into());
    }
    Ok(result)
}
#[cfg(test)]
mod tests;
