//! The response read set of the CloudWatch slice (#4381 repair 4): every
//! response member each command reads, by command, service and operation.
//!
//! Nothing here is a second copy. Each entry names the table its read site
//! uses: the gateway's [`AppSignalsOp::response_fields`], the Logs
//! [`LogsOp::response_fields`], the evaluation's [`EVALUATE_RESPONSE`], the
//! dataset [`control_fields`], the Omni envelope keys of [`omni_sql`] and
//! the span-row tables built from the `ATTR_COLS` column tables. A member
//! added to any of them joins the read set, and the response-mutation
//! harness (`tests/cloudwatch_api.rs`) fails until a fixture carries it.
//!
//! Reviewed exclusions, which are not response members of a table:
//!
//! - The members of the JSON document inside a Logs Insights `@message`
//!   value: the value is one string (`results[*][*].value`), which the
//!   snapshot parser decodes as the record the command prints. The harness
//!   mutates that whole string (#4742 round 4 accepted this boundary).
//! - The members of an attribute-filter group (`AttributeFilters[*]`): their
//!   names are caller data, not API members, so the path stops at the group.
//! - The `error` of a recorded failure step (`code`, `message`): it is the
//!   service error itself, validated by `toolkit_aws::recorded`, not a
//!   response member.
//! - The Logs writeback calls of `evaluate-traces` (`CreateLogGroup`,
//!   `PutRetentionPolicy`, `CreateLogStream`, `PutLogEvents`) and the
//!   `DeleteInstrumentationConfiguration` and `StopTelemetryQuerySession`
//!   responses: no member of them is read.

use super::aws_client::Service;
use super::dataset::{self, control_fields};
use super::evaluate::{self, EVALUATE_RESPONSE};
use super::gateway::AppSignalsOp;
use super::logs_query::LogsOp;
use super::omni_sql;
use super::response_shape::{Field, Shape};

/// One response a command reads, and the members it reads from it.
#[derive(Debug, Clone, Copy)]
pub struct ResponseRead {
    /// The command (`instrumentation`, `snapshots`, `evaluate-traces`,
    /// `capture-dataset`).
    pub command: &'static str,
    /// The service as a recording names it.
    pub service: &'static str,
    /// The API operation.
    pub operation: &'static str,
    /// The members read, with their shapes.
    pub fields: &'static [Field],
}

const OMNI_SESSION: &[Field] = &[(omni_sql::SESSION_ID, Shape::Text)];
const OMNI_START: &[Field] = &[(omni_sql::QUERY_ID, Shape::Text)];
const EVALUATE_RESULTS: &[Field] = &[
    (omni_sql::STATUS, Shape::Text),
    (omni_sql::ROWS, Shape::ObjectList(evaluate::SPAN_ROW)),
    (omni_sql::NEXT_TOKEN, Shape::NullableText),
];
const CAPTURE_RESULTS: &[Field] = &[
    (omni_sql::STATUS, Shape::Text),
    (omni_sql::ROWS, Shape::ObjectList(dataset::SPAN_ROW)),
    (omni_sql::NEXT_TOKEN, Shape::NullableText),
];

fn omni(command: &'static str, results: &'static [Field]) -> [ResponseRead; 3] {
    let service = Service::CloudWatchOmni.name();
    [
        ResponseRead {
            command,
            service,
            operation: "StartTelemetryQuerySession",
            fields: OMNI_SESSION,
        },
        ResponseRead {
            command,
            service,
            operation: "StartTelemetryQuery",
            fields: OMNI_START,
        },
        ResponseRead {
            command,
            service,
            operation: "GetTelemetryQueryResults",
            fields: results,
        },
    ]
}

/// Every response the slice's commands read, with the members read.
#[must_use]
pub fn response_reads() -> Vec<ResponseRead> {
    let mut reads: Vec<ResponseRead> = AppSignalsOp::ALL
        .iter()
        .map(|op| ResponseRead {
            command: "instrumentation",
            service: Service::ApplicationSignals.name(),
            operation: op.api_name(),
            fields: op.response_fields(),
        })
        .collect();
    reads.extend(LogsOp::ALL.iter().map(|op| ResponseRead {
        command: "snapshots",
        service: Service::Logs.name(),
        operation: op.api_name(),
        fields: op.response_fields(),
    }));
    reads.extend(omni("evaluate-traces", EVALUATE_RESULTS));
    reads.push(ResponseRead {
        command: "evaluate-traces",
        service: Service::BedrockAgentCore.name(),
        operation: "Evaluate",
        fields: EVALUATE_RESPONSE,
    });
    reads.extend(omni("capture-dataset", CAPTURE_RESULTS));
    for operation in [
        "ListDatasets",
        "GetDataset",
        "CreateDataset",
        "AddDatasetExamples",
    ] {
        reads.push(ResponseRead {
            command: "capture-dataset",
            service: Service::BedrockAgentCoreControl.name(),
            operation,
            fields: control_fields(operation),
        });
    }
    reads.retain(|read| !read.fields.is_empty());
    reads
}
