//! `query_metering.py`: the metering records of the last 90 days from the
//! CloudTrail event history (`LookupEvents` of `metering-marketplace`), with
//! the filters, the aggregations and the completeness signals of the script.
//! The events come through the client seam (recorded or no-live).

use toolkit_aws::next_token_from;
use toolkit_aws::recorded::{
    list_value, object_members, optional_member, optional_object_list, optional_text,
};
use toolkit_core::{parse_json, Json};

use super::base::Failure;
use super::client::{AwsClient, Service};
use super::clock::python_str;
use crate::cloudwatch::timefmt::UtcTime;
use crate::dynamodb::pyfmt::{text, truthy};

const EVENT_SOURCE: &str = "metering-marketplace.amazonaws.com";
const MAX_RESULTS_PER_PAGE: i64 = 50;
const MICROS_PER_DAY: i64 = 86_400 * 1_000_000;
const HINT: &str = "For data beyond 90 days, check Seller Reports at https://aws.amazon.com/marketplace/management/reports/";

/// The parsed options of `query-metering`.
pub(super) struct QueryArgs {
    pub(super) region: String,
    pub(super) days: i64,
    pub(super) start_date: Option<String>,
    pub(super) end_date: Option<String>,
    pub(super) customer_id: Option<String>,
    pub(super) status: Option<String>,
    pub(super) dimension: Option<String>,
    pub(super) metering_record_id: Option<String>,
    pub(super) product_code: Option<String>,
    pub(super) deployment_product_code: Option<String>,
    pub(super) query_type: String,
    pub(super) top_n: i64,
    pub(super) limit: i64,
    pub(super) max_pages: i64,
}

// ---- numbers -------------------------------------------------------------------

/// A CloudTrail quantity: an integer, or a float once one was seen (Python's
/// `int + float`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    #[allow(clippy::cast_precision_loss)]
    fn as_f64(self) -> f64 {
        match self {
            Self::Int(number) => number as f64,
            Self::Float(number) => number,
        }
    }

    fn plus(self, other: Self) -> Self {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => a
                .checked_add(b)
                .map_or_else(|| Self::Float(self.as_f64() + other.as_f64()), Self::Int),
            _ => Self::Float(self.as_f64() + other.as_f64()),
        }
    }

    fn to_json(self) -> Json {
        match self {
            Self::Int(number) => Json::Int(number),
            Self::Float(number) => Json::Float(number),
        }
    }
}

fn number_of(value: Option<&Json>) -> Result<Num, Failure> {
    match value {
        None => Ok(Num::Int(0)),
        Some(Json::Int(number)) => Ok(Num::Int(*number)),
        Some(Json::Float(number)) => Ok(Num::Float(*number)),
        Some(_) => Err(Failure::message(
            "invalid CloudTrail event: a usage record quantity is not a number",
        )),
    }
}

// ---- records -------------------------------------------------------------------

/// One metering record of a CloudTrail event.
#[derive(Debug, Clone)]
pub(super) struct Record {
    event_time: String,
    customer_account: String,
    customer_identifier: String,
    license_arn: String,
    product_code: String,
    dimension: String,
    quantity: Num,
    timestamp: Json,
    status: String,
    metering_record_id: String,
    usage_allocations: Json,
}

impl Record {
    fn to_json(&self) -> Json {
        Json::object(vec![
            ("eventTime", Json::string(self.event_time.clone())),
            (
                "customerAWSAccountId",
                Json::string(self.customer_account.clone()),
            ),
            (
                "customerIdentifier",
                Json::string(self.customer_identifier.clone()),
            ),
            ("licenseArn", Json::string(self.license_arn.clone())),
            ("productCode", Json::string(self.product_code.clone())),
            ("dimension", Json::string(self.dimension.clone())),
            ("quantity", self.quantity.to_json()),
            ("timestamp", self.timestamp.clone()),
            ("status", Json::string(self.status.clone())),
            (
                "meteringRecordId",
                Json::string(self.metering_record_id.clone()),
            ),
            ("usageAllocations", self.usage_allocations.clone()),
        ])
    }

    /// `str(timestamp)[:10]` when the timestamp is truthy, else `""`.
    fn day(&self) -> String {
        if truthy(&self.timestamp) {
            text(&self.timestamp).chars().take(10).collect()
        } else {
            String::new()
        }
    }
}

/// `x.get(key, {}) or {}` on a CloudTrail member: absent, `null` and any other
/// falsy value are an empty object; a non-empty value of another type is
/// refused.
fn object_or_empty(parent: &Json, key: &str) -> Result<Json, Failure> {
    match parent.get(key) {
        None => Ok(Json::Object(Vec::new())),
        Some(value) if !truthy(value) => Ok(Json::Object(Vec::new())),
        Some(value) if object_members(value).is_some() => Ok(value.clone()),
        Some(_) => Err(Failure::message(format!(
            "invalid CloudTrail event: {key} must be an object"
        ))),
    }
}

fn list_member<'j>(parent: &'j Json, key: &str) -> Result<&'j [Json], Failure> {
    optional_member(parent, key, "a list", list_value)
        .map(Option::unwrap_or_default)
        .map_err(|error| Failure::message(format!("invalid CloudTrail event: {error}")))
}

/// `str(value)` of a member, `""` when absent.
fn key_part(object: &Json, key: &str) -> String {
    object.get(key).map_or_else(String::new, text)
}

type MatchKey = (String, String, String, String);

fn match_key(usage: &Json) -> MatchKey {
    (
        key_part(usage, "customerAWSAccountId"),
        key_part(usage, "dimension"),
        key_part(usage, "timestamp"),
        key_part(usage, "licenseArn"),
    )
}

/// The `licenseArn -> productCode` mapping of `additionalEventData`, in either
/// accepted shape.
fn license_products(additional: &Json) -> Vec<(String, String)> {
    let raw = ["licenseArnToProductCode", "productCodeByLicenseArn"]
        .iter()
        .filter_map(|name| additional.get(name))
        .find(|value| truthy(value));
    let mut pairs: Vec<(String, String)> = Vec::new();
    let Some(raw) = raw else { return pairs };
    if let Some(members) = object_members(raw) {
        for (key, value) in members {
            pairs.push((key.clone(), text(value)));
        }
    } else if let Some(entries) = list_value(raw) {
        for entry in entries {
            let arn = entry.get("licenseArn").filter(|value| truthy(value));
            if let (Some(_), Some(arn)) = (object_members(entry), arn) {
                let product = entry.get("productCode").map_or_else(String::new, text);
                pairs.push((text(arn), product));
            }
        }
    }
    pairs
}

/// `extract_records_from_event`: the metering records of one CloudTrail event,
/// each with the product code it resolves to (`""` when the event alone does
/// not say).
pub(super) fn extract_records(event_time: &str, event: &Json) -> Result<Vec<Record>, Failure> {
    let request = object_or_empty(event, "requestParameters")?;
    let response = object_or_empty(event, "responseElements")?;
    let additional = object_or_empty(event, "additionalEventData")?;
    let usage_records = list_member(&request, "usageRecords")?;
    let results = list_member(&response, "results")?;
    let products = license_products(&additional);
    let request_product = request
        .get("productCode")
        .filter(|value| truthy(value))
        .map(text)
        .unwrap_or_default();

    let mut by_key: Vec<(MatchKey, &Json)> = Vec::new();
    for result in results {
        let empty = Json::Object(Vec::new());
        let usage = result.get("usageRecord").unwrap_or(&empty);
        let key = match_key(usage);
        match by_key.iter_mut().find(|(known, _)| *known == key) {
            Some(slot) => slot.1 = result,
            None => by_key.push((key, result)),
        }
    }
    let none = Json::Object(Vec::new());
    let mut records = Vec::new();
    for usage in usage_records {
        if object_members(usage).is_none() {
            return Err(Failure::message(
                "invalid CloudTrail event: a usage record is not an object",
            ));
        }
        let result = by_key
            .iter()
            .find(|(known, _)| *known == match_key(usage))
            .map_or(&none, |(_, result)| *result);
        let license_arn = key_part(usage, "licenseArn");
        let product_code = if request_product.is_empty() {
            products
                .iter()
                .rev()
                .find(|(arn, _)| *arn == license_arn)
                .map(|(_, product)| product.clone())
                .unwrap_or_default()
        } else {
            request_product.clone()
        };
        records.push(Record {
            event_time: event_time.to_string(),
            customer_account: key_part(usage, "customerAWSAccountId"),
            customer_identifier: key_part(usage, "customerIdentifier"),
            license_arn,
            product_code,
            dimension: key_part(usage, "dimension"),
            quantity: number_of(usage.get("quantity"))?,
            timestamp: usage
                .get("timestamp")
                .cloned()
                .unwrap_or_else(|| Json::string("")),
            status: result
                .get("status")
                .map_or_else(|| "unknown".to_string(), text),
            metering_record_id: key_part(result, "meteringRecordId"),
            usage_allocations: usage
                .get("usageAllocations")
                .cloned()
                .unwrap_or_else(|| Json::Array(Vec::new())),
        });
    }
    Ok(records)
}

// ---- lookup ----------------------------------------------------------------------

/// How the page-bounded fetch ended.
#[derive(Debug, Default)]
pub(super) struct FetchStats {
    pub(super) pages: i64,
    pub(super) truncated: bool,
}

/// `lookup_events`: the CloudTrail events of the window, at most `max_pages`
/// pages of fifty. `truncated` is set when the cap ended the walk while a
/// `NextToken` remained.
pub(super) fn lookup_events(
    client: &mut dyn AwsClient,
    start_epoch: i64,
    end_epoch: i64,
    max_pages: i64,
) -> Result<(Vec<(String, Json)>, FetchStats), Failure> {
    let mut stats = FetchStats::default();
    let mut found = Vec::new();
    let mut token: Option<String> = None;
    while stats.pages < max_pages {
        let mut members = vec![
            (
                "LookupAttributes",
                Json::Array(vec![Json::object(vec![
                    ("AttributeKey", Json::string("EventSource")),
                    ("AttributeValue", Json::string(EVENT_SOURCE)),
                ])]),
            ),
            ("StartTime", Json::Int(start_epoch)),
            ("EndTime", Json::Int(end_epoch)),
            ("MaxResults", Json::Int(MAX_RESULTS_PER_PAGE)),
        ];
        if let Some(token) = &token {
            members.push(("NextToken", Json::string(token.clone())));
        }
        let response = client.call(Service::CloudTrail, "LookupEvents", &Json::object(members))?;
        stats.pages += 1;
        let events = optional_object_list(&response, "Events")
            .map_err(|error| Failure::message(format!("invalid LookupEvents response: {error}")))?;
        for event in events {
            let body = optional_text(event, "CloudTrailEvent")
                .map_err(|error| {
                    Failure::message(format!("invalid LookupEvents response: {error}"))
                })?
                .unwrap_or("{}");
            // A body that is not JSON, or an event without a time, is skipped.
            let (Ok(parsed), Some(time)) = (parse_json(body), event.get("EventTime")) else {
                continue;
            };
            found.push((event_time_text(time), parsed));
        }
        token = next_token_from(&response, "NextToken")?;
        if token.is_none() {
            break;
        }
        if stats.pages >= max_pages {
            stats.truncated = true;
            break;
        }
    }
    Ok((found, stats))
}

/// `str(event_time)`: a number is an epoch time shown as `str(datetime)`.
fn event_time_text(value: &Json) -> String {
    let seconds = match value {
        Json::Int(seconds) => Some(*seconds as f64),
        Json::Float(seconds) => Some(*seconds),
        _ => None,
    };
    match seconds.and_then(UtcTime::from_epoch_seconds) {
        Some(time) => python_str(time),
        None => text(value),
    }
}

// ---- filtering and aggregation ------------------------------------------------------

fn wanted(filter: Option<&String>) -> Option<&str> {
    filter.map(String::as_str).filter(|value| !value.is_empty())
}

/// `matches_filters`: an empty filter value is no filter, as in the script.
pub(super) fn matches_filters(record: &Record, args: &QueryArgs) -> bool {
    if wanted(args.customer_id.as_ref()).is_some_and(|id| record.customer_account != id) {
        return false;
    }
    if wanted(args.status.as_ref()).is_some_and(|status| record.status != status) {
        return false;
    }
    if wanted(args.dimension.as_ref()).is_some_and(|dimension| record.dimension != dimension) {
        return false;
    }
    if wanted(args.metering_record_id.as_ref()).is_some_and(|id| record.metering_record_id != id) {
        return false;
    }
    if wanted(args.product_code.as_ref()).is_some_and(|code| record.product_code != code) {
        return false;
    }
    let day = record.day();
    if !day.is_empty() {
        if wanted(args.start_date.as_ref()).is_some_and(|start| day.as_str() < start) {
            return false;
        }
        if wanted(args.end_date.as_ref()).is_some_and(|end| day.as_str() > end) {
            return false;
        }
    }
    true
}

/// A `collections.Counter` of counts or quantities, in first-seen order.
struct Counter<T> {
    entries: Vec<(String, T)>,
}

impl<T: Copy> Counter<T> {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn add(&mut self, key: &str, zero: T, plus: impl Fn(T) -> T) {
        match self.entries.iter_mut().find(|(known, _)| known == key) {
            Some(slot) => slot.1 = plus(slot.1),
            None => self.entries.push((key.to_string(), plus(zero))),
        }
    }

    fn get(&self, key: &str) -> Option<T> {
        self.entries
            .iter()
            .find(|(known, _)| known == key)
            .map(|(_, value)| *value)
    }
}

fn counted(counter: &Counter<i64>) -> Json {
    Json::Object(
        counter
            .entries
            .iter()
            .map(|(key, count)| (key.clone(), Json::Int(*count)))
            .collect(),
    )
}

/// `Counter.most_common(n)` by a numeric sort key, descending and stable.
fn most_common<T: Copy>(
    counter: &Counter<T>,
    top_n: i64,
    value: impl Fn(T) -> f64,
) -> Vec<(String, T)> {
    let mut entries: Vec<(String, T)> = counter.entries.clone();
    entries.sort_by(|a, b| value(b.1).total_cmp(&value(a.1)));
    entries.truncate(usize::try_from(top_n).unwrap_or(0));
    entries
}

/// `items[:n]`, with Python's negative slice (all but the last `-n`).
fn prefix<T: Clone>(items: &[T], count: i64) -> Vec<T> {
    let length = i64::try_from(items.len()).unwrap_or(i64::MAX);
    let end = if count >= 0 {
        count.min(length)
    } else {
        (length + count).max(0)
    };
    items[..usize::try_from(end).unwrap_or(0)].to_vec()
}

/// `aggregate`: the output of a query type over the filtered records.
#[allow(clippy::too_many_lines)]
pub(super) fn aggregate(records: &[Record], query_type: &str, top_n: i64, limit: i64) -> Json {
    if records.is_empty() {
        return Json::object(vec![
            ("totalRecords", Json::Int(0)),
            (
                "message",
                Json::string("No records match the given filters."),
            ),
            ("hint", Json::string(HINT)),
        ]);
    }
    let mut total = Num::Int(0);
    let mut status_counts = Counter::<i64>::new();
    let mut dim_counts = Counter::<i64>::new();
    let mut dim_qty = Counter::<Num>::new();
    let mut cust_qty = Counter::<Num>::new();
    let mut cust_counts = Counter::<i64>::new();
    let mut dates: Vec<String> = Vec::new();
    for record in records {
        total = total.plus(record.quantity);
        status_counts.add(&record.status, 0, |n| n + 1);
        dim_counts.add(&record.dimension, 0, |n| n + 1);
    }
    for record in records {
        let quantity = record.quantity;
        dim_qty.add(&record.dimension, Num::Int(0), |n| n.plus(quantity));
        cust_qty.add(&record.customer_account, Num::Int(0), |n| n.plus(quantity));
        cust_counts.add(&record.customer_account, 0, |n| n + 1);
        let day = record.day();
        if !day.is_empty() {
            dates.push(day);
        }
    }
    let base = vec![
        (
            "totalRecords",
            Json::Int(i64::try_from(records.len()).unwrap_or(i64::MAX)),
        ),
        ("totalUsageQuantity", total.to_json()),
    ];
    let by_dimension = || {
        Json::Object(
            dim_counts
                .entries
                .iter()
                .map(|(dimension, records)| {
                    (
                        dimension.clone(),
                        Json::object(vec![
                            ("records", Json::Int(*records)),
                            (
                                "totalQuantity",
                                dim_qty.get(dimension).unwrap_or(Num::Int(0)).to_json(),
                            ),
                        ]),
                    )
                })
                .collect(),
        )
    };
    let by_usage = || {
        Json::Array(
            most_common(&cust_qty, top_n, Num::as_f64)
                .into_iter()
                .map(|(customer, quantity)| {
                    Json::object(vec![
                        ("customer", Json::string(customer)),
                        ("totalQuantity", quantity.to_json()),
                    ])
                })
                .collect(),
        )
    };
    let unique = Json::Int(i64::try_from(cust_counts.entries.len()).unwrap_or(i64::MAX));
    let mut out = base;
    match query_type {
        "summary" => {
            out.push(("byStatus", counted(&status_counts)));
            out.push(("byDimension", by_dimension()));
            out.push(("uniqueCustomers", unique));
            out.push(("topCustomersByUsage", by_usage()));
            out.push((
                "dateRange",
                Json::object(vec![
                    (
                        "earliest",
                        dates
                            .iter()
                            .min()
                            .map_or(Json::Null, |d| Json::string(d.clone())),
                    ),
                    (
                        "latest",
                        dates
                            .iter()
                            .max()
                            .map_or(Json::Null, |d| Json::string(d.clone())),
                    ),
                ]),
            ));
        }
        "top_customers" => {
            out.push(("uniqueCustomers", unique));
            out.push(("topCustomersByUsage", by_usage()));
            out.push((
                "topCustomersByRecordCount",
                Json::Array(
                    most_common(&cust_counts, top_n, |n| {
                        #[allow(clippy::cast_precision_loss)]
                        let value = n as f64;
                        value
                    })
                    .into_iter()
                    .map(|(customer, count)| {
                        Json::object(vec![
                            ("customer", Json::string(customer)),
                            ("records", Json::Int(count)),
                        ])
                    })
                    .collect(),
                ),
            ));
        }
        "count" => {
            out.push(("byStatus", counted(&status_counts)));
            out.push(("byDimension", by_dimension()));
        }
        "list_failures" => {
            let failures: Vec<&Record> = records.iter().filter(|r| r.status != "Success").collect();
            let mut by_status = Counter::<i64>::new();
            for record in &failures {
                by_status.add(&record.status, 0, |n| n + 1);
            }
            out.push((
                "totalFailures",
                Json::Int(i64::try_from(failures.len()).unwrap_or(i64::MAX)),
            ));
            out.push(("byStatus", counted(&by_status)));
            out.push((
                "records",
                Json::Array(
                    prefix(&failures, limit)
                        .into_iter()
                        .map(Record::to_json)
                        .collect(),
                ),
            ));
        }
        _ => {
            let length = i64::try_from(records.len()).unwrap_or(i64::MAX);
            out.push(("showing", Json::Int(limit.min(length))));
            out.push((
                "records",
                Json::Array(prefix(records, limit).iter().map(Record::to_json).collect()),
            ));
        }
    }
    Json::object(out)
}

// ---- the command --------------------------------------------------------------------

/// The windows of a query: the requested range clipped to the 90 days the
/// event history keeps.
pub(super) struct Window {
    pub(super) start: UtcTime,
    pub(super) end: UtcTime,
    pub(super) clipped: bool,
    pub(super) requested_start: UtcTime,
}

/// The refusal printed when the whole range predates the retention window.
pub(super) fn out_of_scope(end: UtcTime) -> Json {
    let date = date_text(end);
    Json::object(vec![
        ("query", Json::string("not-performed")),
        (
            "reason",
            Json::string("out-of-scope: requested range is entirely older than 90 days"),
        ),
        (
            "detail",
            Json::string(format!(
                "The requested range ends {date}, which is older than the 90-day CloudTrail Event History retention window. This tool only queries CloudTrail Event History (the last 90 days) and does not query data older than that."
            )),
        ),
        (
            "recommendation",
            Json::string(
                "For data older than 90 days, use the Seller Reports / Seller Delivery Data Feeds (SDDS) path. This is a seller-run path: complete the one-time 'Set up customer data storage' step in the AWS Marketplace Management Portal, then query/ETL the CSV feeds delivered to your S3 bucket. See references/seller-reports.md.",
            ),
        ),
        (
            "sellerReportsUrl",
            Json::string("https://aws.amazon.com/marketplace/management/reports/"),
        ),
    ])
}

/// `%Y-%m-%d` of a time.
pub(super) fn date_text(time: UtcTime) -> String {
    let c = time.civil();
    format!("{:04}-{:02}-{:02}", c.year, c.month, c.day)
}

/// `%Y-%m-%d %H:%M` of a time.
pub(super) fn minute_text(time: UtcTime) -> String {
    let c = time.civil();
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        c.year, c.month, c.day, c.hour, c.minute
    )
}

/// `%Y-%m-%d %H:%M:%S UTC` of a time.
pub(super) fn second_text(time: UtcTime) -> String {
    let c = time.civil();
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        c.year, c.month, c.day, c.hour, c.minute, c.second
    )
}

/// The retention boundary: ninety days before `now`.
pub(super) fn ninety_days_ago(now: UtcTime) -> UtcTime {
    now.checked_add_micros(-90 * MICROS_PER_DAY).unwrap_or(now)
}

/// The refusal printed when a product-specific query has no
/// `licenseArn -> productCode` mapping.
pub(super) fn no_mapping_refusal() -> Json {
    Json::object(vec![
        ("query", Json::string("not-performed")),
        (
            "reason",
            Json::string("product-specific query unsupported: no LicenseArn->productCode mapping"),
        ),
        (
            "detail",
            Json::string(
                "This metering uses LicenseArn-based (Concurrent Agreements) records, and BatchMeterUsage carries no productCode in the API request/response, so a CloudTrail event cannot be attributed to a product from the API fields alone. No product mapping source was available: no --deployment-product-code (single-product stack context), no LicenseArn->productCode mapping in the events' additionalEventData, and no request-level ProductCode (legacy).",
            ),
        ),
        (
            "howToEnable",
            Json::string(
                "Provide --deployment-product-code <productCode> if these events come from a single-product stack, or query a legacy ProductCode-based product (whose events carry ProductCode directly). A subscribers-table lookup (licenseArn -> productCode) is another valid mapping source.",
            ),
        ),
        (
            "supportedNonProductQueries",
            Json::Array(
                [
                    "by customer (--customer-id)",
                    "by dimension (--dimension)",
                    "by status (--status)",
                    "by time range (--start-date/--end-date/--days)",
                ]
                .into_iter()
                .map(Json::string)
                .collect(),
            ),
        ),
    ])
}

/// Stamps `--deployment-product-code` on records that resolved none, and
/// reports whether any record now has a product code.
pub(super) fn apply_deployment_product(records: &mut [Record], deployment: Option<&str>) -> bool {
    if let Some(code) = deployment.filter(|code| !code.is_empty()) {
        for record in records.iter_mut() {
            if record.product_code.is_empty() {
                record.product_code = code.to_string();
            }
        }
    }
    records.iter().any(|record| !record.product_code.is_empty())
}

/// The completeness members added to the output.
pub(super) fn completeness(
    output: &Json,
    args: &QueryArgs,
    window: &Window,
    stats: &FetchStats,
    events_processed: usize,
    records_fetched: usize,
) -> Json {
    let mut members: Vec<(String, Json)> = object_members(output)
        .map(<[(String, Json)]>::to_vec)
        .unwrap_or_default();
    let mut put = |key: &str, value: Json| members.push((key.to_string(), value));
    put("source", Json::string("CloudTrail Event History"));
    put("region", Json::string(args.region.clone()));
    put(
        "timeRange",
        Json::object(vec![
            ("start", Json::string(second_text(window.start))),
            ("end", Json::string(second_text(window.end))),
        ]),
    );
    put("complete", Json::Bool(!(stats.truncated || window.clipped)));
    put("truncated", Json::Bool(stats.truncated));
    put("windowClipped", Json::Bool(window.clipped));
    put(
        "fetch",
        Json::object(vec![
            ("pagesFetched", Json::Int(stats.pages)),
            ("maxPages", Json::Int(args.max_pages)),
            (
                "eventsProcessed",
                Json::Int(i64::try_from(events_processed).unwrap_or(i64::MAX)),
            ),
            (
                "recordsFetched",
                Json::Int(i64::try_from(records_fetched).unwrap_or(i64::MAX)),
            ),
        ]),
    );
    let aggregate_type = matches!(
        args.query_type.as_str(),
        "summary" | "count" | "top_customers"
    );
    let mut warning: Option<String> = None;
    if window.clipped {
        put(
            "windowClippedNote",
            Json::string(format!(
                "PARTIAL WINDOW: the requested start predates the 90-day CloudTrail Event History retention limit, so this covers only {} onward (the requested start was {}). For the older portion use Seller Reports / SDDS - do NOT present this as covering the full requested range.",
                date_text(window.start),
                date_text(window.requested_start)
            )),
        );
    }
    if stats.truncated {
        put(
            "completenessNote",
            Json::string(format!(
                "PARTIAL RESULT: the --max-pages cap ({}) was reached with more CloudTrail events available, so this covers only the fetched sample of the window.",
                args.max_pages
            )),
        );
        if aggregate_type {
            warning = Some("Totals/rankings here are a LOWER BOUND over the fetched sample and are NOT authoritative. Narrow the window (shorter range or add --customer-id/--dimension/--status), raise --max-pages, or use Seller Reports (billed amounts) for an authoritative total.".to_string());
        }
    }
    if window.clipped && aggregate_type && warning.is_none() {
        warning = Some("Totals/rankings cover only the in-retention window (from 90 days ago), NOT the full requested range - they are a LOWER BOUND. Use Seller Reports for the older portion.".to_string());
    }
    if let Some(warning) = warning {
        put("aggregateWarning", Json::string(warning));
    }
    Json::Object(members)
}
