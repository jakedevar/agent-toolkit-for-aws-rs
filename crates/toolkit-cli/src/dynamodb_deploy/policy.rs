//! The IAM documents and the Lambda request of the benchmark (security
//! surface of this slice, see `docs/migration/dynamodb-deploy.md`).
//!
//! Nothing here is sent anywhere: the documents are rendered into the offline
//! plan so they can be reviewed before any future live deploy applies them.
//! Every interpolated value is validated first (account: 12 digits, region and
//! prefix: [`super::names`]), so none can add a wildcard, a statement or a
//! quote to a document.

use toolkit_core::Json;

use crate::dynamodb::pyfmt;

/// Lambda runtime of the benchmark function. The handler source is a Python
/// payload of a managed AWS runtime, an explicit compatibility question
/// recorded in the migration document.
pub(crate) const LAMBDA_RUNTIME: &str = "python3.12";
pub(crate) const LAMBDA_HANDLER: &str = "benchmark.handler";
/// 1769 MB is where Lambda allocates a full vCPU.
pub(crate) const DEFAULT_LAMBDA_MEMORY_MB: i64 = 1769;
pub(crate) const DEFAULT_LAMBDA_TIMEOUT_S: i64 = 900;
/// First botocore release with the vector operations; a vector benchmark must
/// carry an SDK at least this new.
pub(crate) const VECTOR_SDK_MIN: &str = "1.43.64";

fn strings(items: &[&str]) -> Json {
    Json::Array(items.iter().map(|item| Json::string(*item)).collect())
}

/// The role trust policy: only the Lambda service may assume the role, and
/// only for this account (`aws:SourceAccount` guards the confused deputy).
pub(crate) fn lambda_trust_policy(account: &str) -> Json {
    Json::object(vec![
        ("Version", Json::string("2012-10-17")),
        (
            "Statement",
            Json::Array(vec![Json::object(vec![
                ("Effect", Json::string("Allow")),
                (
                    "Principal",
                    Json::object(vec![("Service", Json::string("lambda.amazonaws.com"))]),
                ),
                ("Action", Json::string("sts:AssumeRole")),
                (
                    "Condition",
                    Json::object(vec![(
                        "StringEquals",
                        Json::object(vec![("aws:SourceAccount", Json::string(account))]),
                    )]),
                ),
            ])]),
        ),
    ])
}

/// The inline role policy: data-plane access to the run's own tables, a
/// separate unconditioned statement for `SearchVectors` (the fine-grained
/// condition keys are absent from its request context, so a conditioned
/// statement would deny it), and log writes to the run's own log groups.
pub(crate) fn lambda_role_policy(prefix: &str, region: &str, account: &str) -> Json {
    let table_arn = format!("arn:aws:dynamodb:{region}:{account}:table/{prefix}*");
    let index_arn = format!("{table_arn}/index/*");
    Json::object(vec![
        ("Version", Json::string("2012-10-17")),
        (
            "Statement",
            Json::Array(vec![
                Json::object(vec![
                    ("Effect", Json::string("Allow")),
                    (
                        "Action",
                        strings(&[
                            "dynamodb:DescribeTable",
                            "dynamodb:GetItem",
                            "dynamodb:Query",
                            "dynamodb:Scan",
                            "dynamodb:BatchGetItem",
                            "dynamodb:PutItem",
                            "dynamodb:UpdateItem",
                            "dynamodb:DeleteItem",
                            "dynamodb:BatchWriteItem",
                            "dynamodb:TransactWriteItems",
                            "dynamodb:TransactGetItems",
                        ]),
                    ),
                    (
                        "Resource",
                        strings(&[table_arn.as_str(), index_arn.as_str()]),
                    ),
                ]),
                Json::object(vec![
                    ("Effect", Json::string("Allow")),
                    ("Action", strings(&["dynamodb:SearchVectors"])),
                    ("Resource", strings(&[index_arn.as_str()])),
                ]),
                Json::object(vec![
                    ("Effect", Json::string("Allow")),
                    (
                        "Action",
                        strings(&[
                            "logs:CreateLogGroup",
                            "logs:CreateLogStream",
                            "logs:PutLogEvents",
                        ]),
                    ),
                    (
                        "Resource",
                        Json::string(format!(
                            "arn:aws:logs:{region}:{account}:log-group:/aws/lambda/{prefix}*:*"
                        )),
                    ),
                ]),
            ]),
        ),
    ])
}

/// A policy document exactly as the API receives it: `json.dumps` text.
pub(crate) fn document_text(policy: &Json) -> String {
    pyfmt::dumps(policy, false)
}
