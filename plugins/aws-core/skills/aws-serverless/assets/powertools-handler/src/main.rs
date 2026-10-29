//! Lambda entry point: builds the collaborators once per execution
//! environment and serves invocations. Build and ship it with cargo-lambda
//! (see the aws-serverless skill, `references/deployment.md`).

use std::sync::Arc;

use lambda_runtime::{service_fn, Error, LambdaEvent};
use powertools_handler::{
    metrics, Clock, DynamoDbPersistence, Handler, HandlerSettings, Idempotency, IdempotencyConfig,
    InvocationContext, LineSink, Logger, StdoutSink, SystemClock, Tracer, UdpSegmentSink,
};
use serde_json::Value;

/// The table name the Python template hard-coded; set `IDEMPOTENCY_TABLE` to
/// use another one.
const DEFAULT_TABLE: &str = "IdempotencyTable";

fn invocation_context(event: &lambda_runtime::Context) -> InvocationContext {
    InvocationContext {
        function_name: event.env_config.function_name.clone(),
        memory_size_mb: event.env_config.memory,
        function_arn: event.invoked_function_arn.clone(),
        request_id: event.request_id.clone(),
        xray_trace_id: event.xray_trace_id.clone(),
        deadline_ms: event.deadline,
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let sink: Arc<dyn LineSink> = Arc::new(StdoutSink);
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let logger = Logger::from_env(sink.clone(), clock.clone());
    let service = logger.service().to_string();

    let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .load()
        .await;
    let table = std::env::var("IDEMPOTENCY_TABLE").unwrap_or_else(|_| DEFAULT_TABLE.to_string());
    let store = DynamoDbPersistence::new(aws_sdk_dynamodb::Client::new(&config), &table);

    let handler = Arc::new(Handler::new(
        logger,
        Tracer::new(
            UdpSegmentSink::from_env()
                .map(|s| Arc::new(s) as Arc<dyn powertools_handler::SegmentSink>),
            clock.clone(),
        ),
        Idempotency::new(store, IdempotencyConfig::for_body(), clock.clone()),
        sink,
        clock,
        &service,
        HandlerSettings {
            metrics_service: metrics::service_from_env(),
            ..HandlerSettings::default()
        },
    ));

    lambda_runtime::run(service_fn(move |event: LambdaEvent<Value>| {
        let handler = Arc::clone(&handler);
        async move {
            let (payload, context) = event.into_parts();
            handler
                .handle(payload, &invocation_context(&context))
                .await
                .map_err(Error::from)
        }
    }))
    .await
}
