//! The AWS transport the analyzers read through, and the parsers for what it
//! returns.
//!
//! The Python scripts called `boto3` and `urllib` inline. Here every remote
//! read is one method of [`AwsApi`] that returns already-paginated raw payloads
//! (Pricing API `PriceList` strings, bulk CSV text, RDS / Savings Plans / EC2
//! / `CloudWatch` records as [`Json`]). All filtering, parsing and arithmetic on
//! those payloads is Rust in this crate, so tests feed canned payloads through
//! a mock and never touch AWS.
//!
//! This build ships [`NoAws`], the transport for "`boto3` is not installed":
//! `available()` is false and every call fails. An SDK-backed transport is a
//! separate optional feature (see the slice document); the analyzers behave
//! exactly as the Python did when `boto3` was missing.

use std::collections::HashMap;

use toolkit_core::{parse_json, Json};

use crate::manifests::py_repr_str;

use super::args::parse_py_float;

/// Result of a transport call; the error is the failure text.
pub(crate) type ApiResult<T> = Result<T, String>;

/// Failure text used by [`NoAws`].
pub(crate) const NO_AWS: &str = "AWS SDK access is not available in this build";

/// One EC2 instance type with its CPU and memory.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Ec2Type {
    /// EC2 name without the `db.` prefix (`r6g.large`).
    pub name: String,
    pub vcpus: i64,
    pub memory_mib: f64,
}

/// What `describe_db_clusters` reports for one cluster.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClusterInfo {
    /// `StorageType`, `"aurora"` when the API omits it.
    pub storage_type: String,
    pub engine: String,
    pub engine_version: String,
    /// `DBInstanceIdentifier` of every `DBClusterMembers` entry.
    pub members: Vec<String>,
}

/// Remote reads used by the analyzers.
pub(crate) trait AwsApi {
    /// Whether the SDK is usable at all (`import boto3` succeeded).
    fn available(&self) -> bool;
    /// All pages of Pricing API `get_products` for `AmazonRDS` with the given
    /// `TERM_MATCH` (field, value) filters; each item is a JSON string.
    fn pricing_products(&self, filters: &[(&str, &str)]) -> ApiResult<Vec<String>>;
    /// Body of the public RDS bulk pricing CSV for `region`.
    fn bulk_pricing_csv(&self, region: &str) -> ApiResult<String>;
    /// `DBInstanceClass` of every orderable option for the RDS `engine`.
    fn orderable_instance_classes(&self, region: &str, engine: &str) -> ApiResult<Vec<String>>;
    /// EC2 `describe_instance_types` for the named types.
    fn ec2_instance_types(&self, region: &str, names: &[String]) -> ApiResult<Vec<Ec2Type>>;
    /// `ReservedDBInstancesOfferings` for one class and RDS engine (single-AZ).
    fn reserved_offerings(
        &self,
        region: &str,
        instance_type: &str,
        engine: &str,
    ) -> ApiResult<Vec<Json>>;
    /// Savings Plans `searchResults` for the Database plan in `region`.
    fn savings_plan_rates(&self, region: &str, product_description: &str) -> ApiResult<Vec<Json>>;
    /// `(DBClusterIdentifier, Engine)` of every cluster in the region.
    fn list_clusters(&self, region: &str) -> ApiResult<Vec<(String, String)>>;
    /// One cluster, or `None` when `describe_db_clusters` returns none.
    fn describe_cluster(&self, region: &str, cluster_id: &str) -> ApiResult<Option<ClusterInfo>>;
    /// `DBInstanceClass` of the instances returned for one cluster member.
    fn instance_classes(&self, region: &str, instance_id: &str) -> ApiResult<Vec<String>>;
    /// The `statistic` (`Sum` / `Average`) of every hourly `CloudWatch` datapoint
    /// of an `AWS/RDS` cluster metric between the two epoch-second bounds.
    fn metric_values(
        &self,
        region: &str,
        cluster_id: &str,
        metric: &str,
        statistic: &str,
        window: (i64, i64),
    ) -> ApiResult<Vec<f64>>;
}

/// The transport of a build without an AWS SDK (`boto3` missing in Python).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NoAws;

impl AwsApi for NoAws {
    fn available(&self) -> bool {
        false
    }
    fn pricing_products(&self, _filters: &[(&str, &str)]) -> ApiResult<Vec<String>> {
        Err(NO_AWS.to_string())
    }
    fn bulk_pricing_csv(&self, _region: &str) -> ApiResult<String> {
        Err(NO_AWS.to_string())
    }
    fn orderable_instance_classes(&self, _region: &str, _engine: &str) -> ApiResult<Vec<String>> {
        Err(NO_AWS.to_string())
    }
    fn ec2_instance_types(&self, _region: &str, _names: &[String]) -> ApiResult<Vec<Ec2Type>> {
        Err(NO_AWS.to_string())
    }
    fn reserved_offerings(
        &self,
        _region: &str,
        _instance_type: &str,
        _engine: &str,
    ) -> ApiResult<Vec<Json>> {
        Err(NO_AWS.to_string())
    }
    fn savings_plan_rates(
        &self,
        _region: &str,
        _product_description: &str,
    ) -> ApiResult<Vec<Json>> {
        Err(NO_AWS.to_string())
    }
    fn list_clusters(&self, _region: &str) -> ApiResult<Vec<(String, String)>> {
        Err(NO_AWS.to_string())
    }
    fn describe_cluster(&self, _region: &str, _cluster_id: &str) -> ApiResult<Option<ClusterInfo>> {
        Err(NO_AWS.to_string())
    }
    fn instance_classes(&self, _region: &str, _instance_id: &str) -> ApiResult<Vec<String>> {
        Err(NO_AWS.to_string())
    }
    fn metric_values(
        &self,
        _region: &str,
        _cluster_id: &str,
        _metric: &str,
        _statistic: &str,
        _window: (i64, i64),
    ) -> ApiResult<Vec<f64>> {
        Err(NO_AWS.to_string())
    }
}

/// `import boto3` at the top of a fetcher.
pub(crate) fn ensure_available(api: &dyn AwsApi) -> ApiResult<()> {
    if api.available() {
        Ok(())
    } else {
        Err(NO_AWS.to_string())
    }
}

/// Python `float(value)` for a JSON value, with Python's failure text.
pub(crate) fn json_float(value: &Json) -> ApiResult<f64> {
    match value {
        Json::Int(number) => Ok(*number as f64),
        Json::Float(number) => Ok(*number),
        Json::Bool(flag) => Ok(f64::from(u8::from(*flag))),
        Json::String(text) => parse_py_float(text)
            .ok_or_else(|| format!("could not convert string to float: {}", py_repr_str(text))),
        other => Err(format!(
            "float() argument must be a string or a real number, not '{}'",
            match other {
                Json::Null => "NoneType",
                Json::Array(_) => "list",
                _ => "dict",
            }
        )),
    }
}

/// Parses one Pricing API `PriceList` entry.
pub(crate) fn parse_price_item(raw: &str) -> ApiResult<Json> {
    parse_json(raw).map_err(|error| error.to_string())
}

/// `item["product"]["attributes"][key]`, `""` when absent.
pub(crate) fn attribute<'a>(item: &'a Json, key: &str) -> &'a str {
    item.get("product")
        .and_then(|product| product.get("attributes"))
        .and_then(|attributes| attributes.get(key))
        .and_then(Json::as_str)
        .unwrap_or("")
}

/// Every price dimension of every `OnDemand` term, in document order.
pub(crate) fn on_demand_dimensions(item: &Json) -> Vec<&Json> {
    let mut dimensions = Vec::new();
    if let Some(Json::Object(terms)) = item.get("terms").and_then(|terms| terms.get("OnDemand")) {
        for (_, term) in terms {
            if let Some(Json::Object(prices)) = term.get("priceDimensions") {
                for (_, dimension) in prices {
                    dimensions.push(dimension);
                }
            }
        }
    }
    dimensions
}

/// `float(dimension["pricePerUnit"]["USD"])`, `"0"` when absent.
pub(crate) fn usd_price(dimension: &Json) -> ApiResult<f64> {
    match dimension
        .get("pricePerUnit")
        .and_then(|price| price.get("USD"))
    {
        None => Ok(0.0),
        Some(value) => json_float(value),
    }
}

/// Python `str.splitlines()` boundaries.
fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r'
            | '\u{0b}'
            | '\u{0c}'
            | '\u{1c}'
            | '\u{1d}'
            | '\u{1e}'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

fn split_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if is_line_break(c) {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            lines.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Rows of Python `csv.DictReader` over `text` (default `excel` dialect): the
/// first non-empty row names the columns, blank rows are skipped, a short row
/// leaves later columns absent and extra cells are dropped.
pub(crate) fn parse_csv_rows(text: &str) -> Vec<HashMap<String, String>> {
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut field_started = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' && field.is_empty() {
            in_quotes = true;
            field_started = true;
        } else if c == ',' {
            record.push(std::mem::take(&mut field));
            field_started = true;
        } else if c == '\n' || c == '\r' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            if field_started || !field.is_empty() || !record.is_empty() {
                record.push(std::mem::take(&mut field));
            }
            records.push(std::mem::take(&mut record));
            field_started = false;
        } else {
            field.push(c);
            field_started = true;
        }
    }
    if field_started || !field.is_empty() || !record.is_empty() {
        record.push(field);
    }
    if !record.is_empty() {
        records.push(record);
    }
    let mut rows = records.into_iter().filter(|row| !row.is_empty());
    let Some(header) = rows.next() else {
        return Vec::new();
    };
    rows.map(|row| {
        header
            .iter()
            .cloned()
            .zip(row)
            .collect::<HashMap<String, String>>()
    })
    .collect()
}

/// The bulk-CSV body as the Python read it: metadata lines before the `SKU`
/// header are dropped and line endings normalized, then parsed as CSV.
pub(crate) fn parse_bulk_csv(raw: &str) -> Vec<HashMap<String, String>> {
    let lines = split_lines(raw);
    let header_index = lines
        .iter()
        .position(|line| line.starts_with("\"SKU\"") || line.starts_with("SKU"))
        .unwrap_or(0);
    parse_csv_rows(&lines[header_index..].join("\n"))
}

/// Aurora cluster identifiers (`Engine` starts with `aurora`).
pub(crate) fn aurora_cluster_ids(api: &dyn AwsApi, region: &str) -> ApiResult<Vec<String>> {
    Ok(api
        .list_clusters(region)?
        .into_iter()
        .filter(|(_, engine)| engine.starts_with("aurora"))
        .map(|(id, _)| id)
        .collect())
}
