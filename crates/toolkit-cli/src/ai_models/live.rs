//! Generated SDK codecs over toolkit-aws's guarded service bridge.
use super::client::{Operation, SageMakerClient};
use aws_smithy_runtime_api::client::auth::AuthSchemeId;
use aws_smithy_types::{retry::RetryConfig, timeout::TimeoutConfig, DateTime};
use std::collections::BTreeMap;
#[cfg(test)]
use std::sync::Arc;
use toolkit_aws::{sdk::service_bridge::ServiceBridge, AwsError};
use toolkit_aws::recorded::{optional_object, optional_object_list, optional_text as optional_string, MemberError};
use toolkit_core::Json;
#[cfg(test)]
type TestSender = Arc<
    dyn Fn(toolkit_aws::HttpRequest) -> Result<toolkit_aws::HttpResponse, AwsError> + Send + Sync,
>;

pub(super) struct LiveClient {
    region: Option<String>,
    bridges: BTreeMap<&'static str, ServiceBridge>,
    #[cfg(test)]
    sender: Option<TestSender>,
}
impl LiveClient {
    // Selecting this client never resolves credentials or opens sockets.
    pub(super) fn new(region: Option<&str>) -> Self {
        Self {
            region: region.map(str::to_string),
            bridges: BTreeMap::new(),
            #[cfg(test)]
            sender: None,
        }
    }
    #[cfg(test)]
    fn with_sender(
        sender: impl Fn(toolkit_aws::HttpRequest) -> Result<toolkit_aws::HttpResponse, AwsError>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            sender: Some(Arc::new(sender)),
            ..Self::new(Some("us-west-2"))
        }
    }
    fn bridge(&mut self, service: &'static str) -> Result<&mut ServiceBridge, AwsError> {
        if !self.bridges.contains_key(service) {
            #[cfg(test)]
            let bridge = if let Some(sender) = &self.sender {
                let sender = sender.clone();
                let config = toolkit_aws::ClientConfig::resolve(
                    service,
                    &toolkit_aws::ClientSettings {
                        region: self.region.as_deref(),
                        retry: toolkit_aws::RetryPolicy::none(),
                        ..toolkit_aws::ClientSettings::default()
                    },
                    &toolkit_aws::EndpointPolicy::aws_public(),
                    &toolkit_aws::MapEnv::new(),
                    &toolkit_aws::testing::FixedResolver::new(),
                )?;
                ServiceBridge::with_sender(config, move |request, _| sender(request))?
            } else {
                ServiceBridge::new(service, self.region.as_deref())?
            };
            #[cfg(not(test))]
            let bridge = ServiceBridge::new(service, self.region.as_deref())?;
            self.bridges.insert(service, bridge);
        }
        self.bridges
            .get_mut(service)
            .ok_or_else(|| AwsError::invalid_config("client", "service client was not initialized"))
    }
}
// Same injected-connector/noAuth contract proved by docgraph's SDK spike.
// recorded-shape: this macro only constructs typed SDK configuration; it reads no JSON, recording or response members.
macro_rules! sdk_client {
    ($sdk:ident, $bridge:expr) => {
        $sdk::Client::from_conf(
            $sdk::Config::builder()
                .behavior_version_latest()
                .region($sdk::config::Region::new(
                    $bridge.config().region().name().to_string(),
                ))
                .endpoint_url($bridge.config().endpoint().url())
                .http_client($bridge.http_client())
                .identity_cache($sdk::config::IdentityCache::no_cache())
                .allow_no_auth()
                .auth_scheme_preference([AuthSchemeId::new("noAuth")])
                .retry_config(RetryConfig::disabled())
                .timeout_config(TimeoutConfig::disabled())
                .build(),
        )
    };
}
impl SageMakerClient for LiveClient {
    fn check_search_support(&self) -> Result<(), String> {
        if aws_sdk_sagemaker::types::ResourceType::values().contains(&"HubContent") {
            Ok(())
        } else {
            Err("the linked SageMaker SDK does not support HubContent Search".to_string())
        }
    }
    fn call(&mut self, operation: Operation, request: &Json) -> Result<Json, AwsError> {
        match operation {
            Operation::GetMetricData => metrics(self.bridge("monitoring")?, request),
            Operation::FilterLogEvents => logs(self.bridge("logs")?, request),
            _ => sagemaker(self.bridge("sagemaker")?, operation, request),
        }
    }
}
fn text<'a>(value: &'a Json, key: &str) -> Result<&'a str, AwsError> {
    value
        .get(key)
        .and_then(Json::as_str)
        .ok_or_else(|| AwsError::invalid_config("request", format!("{key} must be a string")))
}
fn request_shape(error: MemberError) -> AwsError {
    AwsError::invalid_config("request", error.to_string())
}
fn integer(value: &Json, key: &str) -> Result<i64, AwsError> {
    match value.get(key) {
        Some(Json::Int(value)) => Ok(*value),
        _ => Err(AwsError::invalid_config(
            "request",
            format!("{key} must be an integer"),
        )),
    }
}
fn int32(value: &Json, key: &str) -> Result<i32, AwsError> {
    i32::try_from(integer(value, key)?)
        .map_err(|_| AwsError::invalid_config("request", format!("{key} is out of range")))
}
fn strings(values: &[String]) -> Json {
    Json::Array(values.iter().map(Json::string).collect())
}
fn string(value: Option<&str>) -> Json {
    value.map_or(Json::Null, Json::string)
}
fn timestamp(value: Option<&DateTime>) -> Json {
    value.map_or(Json::Null, |value| Json::Float(value.as_secs_f64()))
}
fn token(members: &mut Vec<(&str, Json)>, value: Option<&str>) {
    if let Some(value) = value {
        members.push(("NextToken", Json::string(value)));
    }
}
fn filters(value: &Json) -> Result<Vec<aws_sdk_sagemaker::types::Filter>, AwsError> {
    optional_object_list(value, "Filters").map_err(request_shape)?
        .iter()
        .map(|filter| {
            Ok(aws_sdk_sagemaker::types::Filter::builder()
                .name(text(filter, "Name")?)
                .operator(aws_sdk_sagemaker::types::Operator::from(text(
                    filter, "Operator",
                )?))
                .set_value(optional_string(filter, "Value").map_err(request_shape)?.map(str::to_string))
                .build())
        })
        .collect()
}
fn expression(
    value: &Json,
    depth: usize,
) -> Result<aws_sdk_sagemaker::types::SearchExpression, AwsError> {
    if depth >= 64 {
        return Err(AwsError::invalid_config(
            "request",
            "SearchExpression is too deep",
        ));
    }
    let nested = optional_object_list(value, "NestedFilters").map_err(request_shape)?
        .iter()
        .map(|nested| {
            Ok(aws_sdk_sagemaker::types::NestedFilters::builder()
                .nested_property_name(text(nested, "NestedPropertyName")?)
                .set_filters(Some(filters(nested)?))
                .build())
        })
        .collect::<Result<Vec<_>, AwsError>>()?;
    let sub = optional_object_list(value, "SubExpressions").map_err(request_shape)?
        .iter()
        .map(|sub| expression(sub, depth + 1))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(aws_sdk_sagemaker::types::SearchExpression::builder()
        .set_operator(
            optional_string(value, "Operator").map_err(request_shape)?
                .map(aws_sdk_sagemaker::types::BooleanOperator::from),
        )
        .set_filters(
            (!optional_object_list(value, "Filters").map_err(request_shape)?.is_empty())
                .then(|| filters(value))
                .transpose()?,
        )
        .set_nested_filters((!nested.is_empty()).then_some(nested))
        .set_sub_expressions((!sub.is_empty()).then_some(sub))
        .build())
}
#[allow(clippy::too_many_lines)] // Keep each operation's request and response mapping together.
fn sagemaker(
    bridge: &mut ServiceBridge,
    operation: Operation,
    request: &Json,
) -> Result<Json, AwsError> {
    let sdk = sdk_client!(aws_sdk_sagemaker, bridge);
    match operation {
        Operation::ListHubContents => {
            let call = sdk
                .list_hub_contents()
                .hub_name(text(request, "HubName")?)
                .hub_content_type(aws_sdk_sagemaker::types::HubContentType::from(text(
                    request,
                    "HubContentType",
                )?))
                .max_results(int32(request, "MaxResults")?)
                .set_next_token(optional_string(request, "NextToken").map_err(request_shape)?.map(str::to_string));
            let output = bridge.run(|| call.clone().send())?;
            let items = output
                .hub_content_summaries()
                .iter()
                .map(|item| {
                    Json::object(vec![
                        ("HubContentName", string(item.hub_content_name())),
                        (
                            "HubContentSearchKeywords",
                            strings(item.hub_content_search_keywords()),
                        ),
                        (
                            "OriginalCreationTime",
                            timestamp(item.original_creation_time()),
                        ),
                    ])
                })
                .collect();
            let mut members = vec![("HubContentSummaries", Json::Array(items))];
            token(&mut members, output.next_token());
            Ok(Json::object(members))
        }
        Operation::DescribeHubContent => {
            let call = sdk
                .describe_hub_content()
                .hub_name(text(request, "HubName")?)
                .hub_content_type(aws_sdk_sagemaker::types::HubContentType::from(text(
                    request,
                    "HubContentType",
                )?))
                .hub_content_name(text(request, "HubContentName")?);
            let output = bridge.run(|| call.clone().send())?;
            Ok(Json::object(vec![
                ("HubContentName", string(output.hub_content_name())),
                (
                    "HubContentSearchKeywords",
                    strings(output.hub_content_search_keywords()),
                ),
                ("HubContentDocument", string(output.hub_content_document())),
            ]))
        }
        Operation::Search => {
            let search = optional_object(request, "SearchExpression").map_err(request_shape)?.ok_or_else(|| {
                AwsError::invalid_config("request", "SearchExpression is required")
            })?;
            let call = sdk
                .search()
                .resource(aws_sdk_sagemaker::types::ResourceType::from(text(
                    request, "Resource",
                )?))
                .search_expression(expression(search, 0)?)
                .max_results(int32(request, "MaxResults")?)
                .sort_by(text(request, "SortBy")?)
                .sort_order(aws_sdk_sagemaker::types::SearchSortOrder::from(text(
                    request,
                    "SortOrder",
                )?))
                .set_next_token(optional_string(request, "NextToken").map_err(request_shape)?.map(str::to_string));
            let output = bridge.run(|| call.clone().send())?;
            let results = output
                .results()
                .iter()
                .map(|record| {
                    let Some(content) = record.hub_content() else {
                        return Json::object(vec![]);
                    };
                    Json::object(vec![(
                        "HubContent",
                        Json::object(vec![
                            ("HubContentName", string(content.hub_content_name())),
                            (
                                "HubContentSearchKeywords",
                                strings(content.hub_content_search_keywords()),
                            ),
                            ("HubContentDocument", string(content.hub_content_document())),
                            ("CreationTime", timestamp(content.creation_time())),
                        ]),
                    )])
                })
                .collect();
            let mut members = vec![("Results", Json::Array(results))];
            token(&mut members, output.next_token());
            Ok(Json::object(members))
        }
        Operation::DescribeEndpoint => {
            let call = sdk
                .describe_endpoint()
                .endpoint_name(text(request, "EndpointName")?);
            let output = bridge.run(|| call.clone().send())?;
            let variants = output
                .production_variants()
                .iter()
                .map(|variant| {
                    Json::object(vec![
                        ("VariantName", string(variant.variant_name())),
                        (
                            "CurrentInstanceCount",
                            variant
                                .current_instance_count()
                                .map_or(Json::Null, |value| Json::Int(i64::from(value))),
                        ),
                        (
                            "CurrentWeight",
                            variant
                                .current_weight()
                                .map_or(Json::Null, |value| Json::Float(f64::from(value))),
                        ),
                    ])
                })
                .collect();
            Ok(Json::object(vec![
                (
                    "EndpointStatus",
                    string(
                        output
                            .endpoint_status()
                            .map(aws_sdk_sagemaker::types::EndpointStatus::as_str),
                    ),
                ),
                ("FailureReason", string(output.failure_reason())),
                ("CreationTime", timestamp(output.creation_time())),
                ("LastModifiedTime", timestamp(output.last_modified_time())),
                ("ProductionVariants", Json::Array(variants)),
            ]))
        }
        _ => Err(AwsError::invalid_config(
            "operation",
            "not a SageMaker operation",
        )),
    }
}
fn metric_queries(
    request: &Json,
) -> Result<Vec<aws_sdk_cloudwatch::types::MetricDataQuery>, AwsError> {
    use aws_sdk_cloudwatch::types::{Dimension, Metric, MetricDataQuery, MetricStat};
    optional_object_list(request, "MetricDataQueries").map_err(request_shape)?
        .iter()
        .map(|query| {
            let stat = optional_object(query, "MetricStat").map_err(request_shape)?
                .ok_or_else(|| AwsError::invalid_config("request", "MetricStat is required"))?;
            let metric = optional_object(stat, "Metric").map_err(request_shape)?
                .ok_or_else(|| AwsError::invalid_config("request", "Metric is required"))?;
            let dimensions = optional_object_list(metric, "Dimensions").map_err(request_shape)?
                .iter()
                .map(|dimension| {
                    Ok(Dimension::builder()
                        .name(text(dimension, "Name")?)
                        .value(text(dimension, "Value")?)
                        .build())
                })
                .collect::<Result<Vec<_>, AwsError>>()?;
            Ok(MetricDataQuery::builder()
                .id(text(query, "Id")?)
                .metric_stat(
                    MetricStat::builder()
                        .metric(
                            Metric::builder()
                                .namespace(text(metric, "Namespace")?)
                                .metric_name(text(metric, "MetricName")?)
                                .set_dimensions(Some(dimensions))
                                .build(),
                        )
                        .period(int32(stat, "Period")?)
                        .stat(text(stat, "Stat")?)
                        .build(),
                )
                .build())
        })
        .collect()
}
fn date(request: &Json, key: &str) -> Result<DateTime, AwsError> {
    crate::cloudwatch::timefmt::parse_iso_timestamp(text(request, key)?)
        .and_then(|time| {
            DateTime::from_nanos(i128::from(time.epoch_micros()) * 1000)
                .map_err(|_| "timestamp is out of range".to_string())
        })
        .map_err(|_| {
            AwsError::invalid_config("request", format!("{key} must be an RFC3339 instant"))
        })
}
fn metrics(bridge: &mut ServiceBridge, request: &Json) -> Result<Json, AwsError> {
    let sdk = sdk_client!(aws_sdk_cloudwatch, bridge);
    let call = sdk
        .get_metric_data()
        .set_metric_data_queries(Some(metric_queries(request)?))
        .start_time(date(request, "StartTime")?)
        .end_time(date(request, "EndTime")?);
    let output = bridge.run(|| call.clone().send())?;
    let results = output
        .metric_data_results()
        .iter()
        .map(|result| {
            Json::object(vec![
                ("Id", string(result.id())),
                (
                    "Values",
                    Json::Array(result.values().iter().copied().map(Json::Float).collect()),
                ),
            ])
        })
        .collect();
    let mut members = vec![("MetricDataResults", Json::Array(results))];
    token(&mut members, output.next_token());
    Ok(Json::object(members))
}
fn logs(bridge: &mut ServiceBridge, request: &Json) -> Result<Json, AwsError> {
    let sdk = sdk_client!(aws_sdk_cloudwatchlogs, bridge);
    let call = sdk
        .filter_log_events()
        .log_group_name(text(request, "logGroupName")?)
        .start_time(integer(request, "startTime")?)
        .end_time(integer(request, "endTime")?)
        .limit(int32(request, "limit")?);
    let output = bridge.run(|| call.clone().send())?;
    let events = output
        .events()
        .iter()
        .map(|event| {
            Json::object(vec![
                ("message", string(event.message())),
                ("timestamp", event.timestamp().map_or(Json::Null, Json::Int)),
            ])
        })
        .collect();
    let mut members = vec![("events", Json::Array(events))];
    if let Some(next) = output.next_token() {
        members.push(("nextToken", Json::string(next)));
    }
    Ok(Json::object(members))
}

#[cfg(test)]
mod tests;
