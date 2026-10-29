//! `metering_core.py`: sort-key and group validation, the month windows, the
//! allocation merge, audit stamps, EMF metrics and the bounded row writes.

use toolkit_aws::recorded::{list_value, object_members, optional_text};
use toolkit_core::Json;

use super::base::{aws_class, service_code, shown, Ctx, Failure};
use super::client::Service;
use super::clock::{
    add_hours, epoch, floor_hour, hour_bucket, parse_hour_bucket, parse_second_stamp, stamp,
    year_month,
};
use super::ddb::{self, query_all, Query, Update};
use crate::cloudwatch::timefmt::UtcTime;
use crate::dynamodb::pyfmt::{dumps, text, truthy};

/// `BatchMeterUsage` hard limit: records per call.
pub(super) const BATCH_SIZE: usize = 25;
/// Per-`UsageRecord` `UsageAllocations` cap.
pub(super) const MAX_ALLOCATIONS_PER_RECORD: usize = 2500;
/// Per-allocation `Tags` cap.
pub(super) const MAX_TAGS_PER_ALLOCATION: usize = 5;
/// Raw-usage row keys per cleanup message.
pub(super) const CLEANUP_KEYS_PER_MESSAGE: usize = 100;
/// Terminal raw-row status of a writer-contract violation.
pub(super) const STATUS_REJECTED: &str = "RejectedClientSide";
/// Terminal aggregated-record status of a request-level submit exception.
pub(super) const STATUS_ERROR: &str = "Error";
/// The deepest request-level-failure bisection: `ceil(log2(BATCH_SIZE))`.
pub(super) const MAX_BISECT_DEPTH: u32 = 5;
/// Sparse deprovisioning index of the subscribers table.
pub(super) const DEPROVISIONING_PENDING_INDEX: &str = "deprovisioning-pending-index";
/// Constant HASH of that index.
pub(super) const DEPROVISIONING_PENDING_VALUE: &str = "1";
/// From 03:00 UTC on the 1st the previous-month lock drops to 0.
pub(super) const MONTH_GRACE_LOCK_DROP_HOUR: i64 = 3;
/// The service stops accepting previous-month usage at 06:00 UTC.
pub(super) const MONTH_GRACE_CUTOFF_HOUR: i64 = 6;

/// `mask`: `***` up to six characters, else the first and last three.
pub(super) fn mask(identifier: &str) -> String {
    let chars: Vec<char> = identifier.chars().collect();
    if chars.len() <= 6 {
        "***".to_string()
    } else {
        let head: String = chars[..3].iter().collect();
        let tail: String = chars[chars.len() - 3..].iter().collect();
        format!("{head}***{tail}")
    }
}

/// `validate_sort_key`: the reject reason of a raw-usage sort key that is not
/// `{account}#{dimension}#{YYYY-MM-DDTHH:MM:SS}`, else `None`.
pub(super) fn validate_sort_key(
    sort_key: &str,
    account_id: &str,
    dimension: &str,
) -> Option<&'static str> {
    if sort_key.is_empty() {
        return Some("MalformedSortKey");
    }
    let parts: Vec<&str> = sort_key.split('#').collect();
    if parts.len() < 3 {
        return Some("MalformedSortKey");
    }
    let key_dimension = parts[1..parts.len() - 1].join("#");
    if parts[0] != account_id || key_dimension != dimension {
        return Some("SortKeyMismatch");
    }
    if parse_second_stamp(parts[parts.len() - 1]).is_none() {
        return Some("MalformedTimestamp");
    }
    None
}

/// `completed_hour_buckets`: now-23h .. now-1h, oldest first.
#[cfg(test)]
pub(super) fn completed_hour_buckets(now: UtcTime) -> Vec<String> {
    let base = floor_hour(now);
    (1..=23)
        .rev()
        .map(|hours| hour_bucket(add_hours(base, -hours)))
        .collect()
}

/// `hour_bucket_for_offset`: `floor(now, hour) - hour_offset`.
pub(super) fn hour_bucket_for_offset(now: UtcTime, hour_offset: i64) -> String {
    hour_bucket(add_hours(floor_hour(now), -hour_offset))
}

/// `is_previous_month_bucket`.
pub(super) fn is_previous_month_bucket(bucket: &str, now: UtcTime) -> bool {
    parse_hour_bucket(bucket).is_some_and(|time| year_month(time) < year_month(now))
}

fn in_month_grace_flush(now: UtcTime) -> bool {
    let c = now.civil();
    c.day == 1 && (MONTH_GRACE_LOCK_DROP_HOUR..MONTH_GRACE_CUTOFF_HOUR).contains(&c.hour)
}

/// `effective_lock_hours`: the configured lock clamped to 1..=20, 0 for a
/// previous-month bucket during the month-end immediate-flush window.
pub(super) fn effective_lock_hours(bucket: &str, now: UtcTime, configured_lock: i64) -> i64 {
    let lock = configured_lock.clamp(1, 20);
    if is_previous_month_bucket(bucket, now) && in_month_grace_flush(now) {
        0
    } else {
        lock
    }
}

/// `previous_month_grace_open`: the 1st before 06:00 UTC.
pub(super) fn previous_month_grace_open(now: UtcTime) -> bool {
    let c = now.civil();
    c.day == 1 && c.hour < MONTH_GRACE_CUTOFF_HOUR
}

// ---- audit stamps -----------------------------------------------------------

const STAMPS: &str = "createdAt = if_not_exists(createdAt, :now), updatedAt = :now";

fn is_clause_keyword(text: &str) -> bool {
    ["ADD", "REMOVE", "DELETE"]
        .iter()
        .any(|keyword| text.eq_ignore_ascii_case(keyword))
}

/// The start of the first `\s(ADD|REMOVE|DELETE)\s` of an expression.
fn first_non_set_clause(expression: &str) -> Option<usize> {
    let chars: Vec<(usize, char)> = expression.char_indices().collect();
    for (at, (offset, c)) in chars.iter().enumerate() {
        if !c.is_whitespace() {
            continue;
        }
        let word: String = chars[at + 1..]
            .iter()
            .map(|(_, c)| *c)
            .take_while(|c| !c.is_whitespace())
            .collect();
        let after = at + 1 + word.chars().count();
        if is_clause_keyword(&word) && chars.get(after).is_some_and(|(_, c)| c.is_whitespace()) {
            return Some(*offset);
        }
    }
    None
}

/// Whether the expression has a `SET` clause: `(^|\s)SET\s`.
fn has_set_clause(expression: &str) -> bool {
    let chars: Vec<char> = expression.chars().collect();
    (0..chars.len()).any(|at| {
        (at == 0 || chars[at - 1].is_whitespace())
            && chars.len() > at + 3
            && chars[at..at + 3]
                .iter()
                .collect::<String>()
                .eq_ignore_ascii_case("SET")
            && chars[at + 3].is_whitespace()
    })
}

/// `with_audit_timestamps`: adds `createdAt` (once) and `updatedAt` inside the
/// `SET` clause, before any `ADD`/`REMOVE`/`DELETE` clause, and `:now` to the
/// values. An expression without a `SET` clause gets one in front.
pub(super) fn with_audit_timestamps(
    expression: &str,
    values: Vec<(String, Json)>,
    now: UtcTime,
) -> (String, Json) {
    let mut values = values;
    let stamped = Json::string(stamp(now));
    match values.iter_mut().find(|(name, _)| name == ":now") {
        Some(slot) => slot.1 = stamped,
        None => values.push((":now".to_string(), stamped)),
    }
    let expression = if has_set_clause(expression) {
        match first_non_set_clause(expression) {
            Some(cut) => format!(
                "{}, {STAMPS}{}",
                expression[..cut].trim_end(),
                &expression[cut..]
            ),
            None => format!("{expression}, {STAMPS}"),
        }
    } else {
        format!("SET {STAMPS} {expression}")
    };
    let object = Json::Object(values);
    (expression, object)
}

/// `audit_item`: `createdAt` kept if present, `updatedAt` always now.
pub(super) fn audit_item(mut members: Vec<(String, Json)>, now: UtcTime) -> Json {
    let stamped = stamp(now);
    if !members.iter().any(|(name, _)| name == "createdAt") {
        members.push(("createdAt".to_string(), Json::string(stamped.clone())));
    }
    match members.iter_mut().find(|(name, _)| name == "updatedAt") {
        Some(slot) => slot.1 = Json::string(stamped),
        None => members.push(("updatedAt".to_string(), Json::string(stamped))),
    }
    Json::Object(members)
}

// ---- numbers ---------------------------------------------------------------

/// How a text reads as a `decimal.Decimal`.
#[derive(Debug, PartialEq, Eq)]
enum Decimal {
    /// An integral value that fits `i64`.
    Integer(i64),
    /// An integral value beyond `i64`.
    Overflow,
    /// A finite fraction, `NaN` or `Infinity`.
    NotIntegral,
    /// Not a number at all.
    Invalid,
}

fn digit_run(text: &str) -> Option<String> {
    if text.is_empty() || text.starts_with('_') || text.ends_with('_') || text.contains("__") {
        return None;
    }
    if !text.chars().all(|c| c.is_ascii_digit() || c == '_') {
        return None;
    }
    Some(text.chars().filter(|c| *c != '_').collect())
}

fn decimal(raw: &str) -> Decimal {
    let trimmed = raw.trim();
    let (negative, body) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let lower = body.to_lowercase();
    if lower == "inf" || lower == "infinity" {
        return Decimal::NotIntegral;
    }
    for special in ["snan", "nan"] {
        if let Some(rest) = lower.strip_prefix(special) {
            if rest.is_empty() || rest.chars().all(|c| c.is_ascii_digit()) {
                return Decimal::NotIntegral;
            }
        }
    }
    let (mantissa, exponent) = match lower.split_once('e') {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (lower.as_str(), None),
    };
    let exponent_value = match exponent {
        None => 0_i64,
        Some(exponent) => {
            let (sign, digits) = match exponent.strip_prefix('-') {
                Some(rest) => (-1, rest),
                None => (1, exponent.strip_prefix('+').unwrap_or(exponent)),
            };
            match digit_run(digits) {
                // Beyond 10^6 the value is huge or tiny whatever the digits.
                Some(run) => sign * run.parse::<i64>().unwrap_or(1_000_000).min(1_000_000),
                None => return Decimal::Invalid,
            }
        }
    };
    let (whole, fraction) = match mantissa.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (mantissa, ""),
    };
    let whole_digits = if whole.is_empty() && !fraction.is_empty() {
        String::new()
    } else {
        match digit_run(whole) {
            Some(run) => run,
            None => return Decimal::Invalid,
        }
    };
    let fraction_digits = if fraction.is_empty() {
        String::new()
    } else {
        match digit_run(fraction) {
            Some(run) => run,
            None => return Decimal::Invalid,
        }
    };
    let digits = format!("{whole_digits}{fraction_digits}");
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Decimal::Integer(0);
    }
    // value = digits * 10^(exponent - len(fraction))
    let scale = exponent_value - i64::try_from(fraction_digits.len()).unwrap_or(i64::MAX);
    let magnitude: Option<i128> = if scale >= 0 {
        if scale > 30 || digits.len() > 30 {
            None
        } else {
            digits
                .parse::<i128>()
                .ok()
                .and_then(|value| value.checked_mul(10_i128.pow(u32::try_from(scale).unwrap_or(0))))
        }
    } else {
        let drop = usize::try_from(-scale).unwrap_or(usize::MAX);
        if drop >= digits.len() {
            return Decimal::NotIntegral;
        }
        let (kept, dropped) = digits.split_at(digits.len() - drop);
        if !dropped.chars().all(|c| c == '0') {
            return Decimal::NotIntegral;
        }
        kept.parse::<i128>().ok()
    };
    match magnitude {
        None => Decimal::Overflow,
        Some(value) => {
            let signed = if negative { -value } else { value };
            i64::try_from(signed).map_or(Decimal::Overflow, Decimal::Integer)
        }
    }
}

/// `strict_int`: the integer value of an exact integer, else `None` (a bool, a
/// fraction, a non-number, or an integer beyond `i64`).
pub(super) fn strict_int(value: &Json) -> Option<i64> {
    match value {
        Json::Int(number) => Some(*number),
        Json::Float(number) => {
            #[allow(clippy::cast_precision_loss)]
            let in_range = *number >= i64::MIN as f64 && *number < i64::MAX as f64;
            #[allow(clippy::cast_possible_truncation)]
            (number.is_finite() && number.fract() == 0.0 && in_range).then_some(*number as i64)
        }
        Json::String(raw) => match decimal(raw) {
            Decimal::Integer(number) => Some(number),
            _ => None,
        },
        _ => None,
    }
}

/// `quantity_reason`: why a raw quantity is not a valid integer.
pub(super) fn quantity_reason(raw: &Json) -> &'static str {
    match raw {
        Json::Float(number) if number.is_finite() && number.fract() == 0.0 => "QuantityOutOfRange",
        Json::Float(_) | Json::Int(_) => "NonIntegerQuantity",
        Json::String(text) => match decimal(text) {
            Decimal::Overflow => "QuantityOutOfRange",
            Decimal::Invalid => "NonNumericQuantity",
            Decimal::Integer(_) | Decimal::NotIntegral => "NonIntegerQuantity",
        },
        _ => "NonNumericQuantity",
    }
}

// ---- aggregation -------------------------------------------------------------

/// One merged tag set of a group: its sorted signature, the tags in first-seen
/// order and the summed quantity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Allocation {
    signature: Vec<(String, String)>,
    pub(super) tags: Vec<(String, String)>,
    pub(super) qty: i64,
}

/// A `(licenseArn, account, dimension, hour)` group folded from its raw rows.
#[derive(Debug, Clone)]
pub(super) struct Group {
    pub(super) license_arn: String,
    pub(super) account_id: String,
    pub(super) dimension: String,
    pub(super) hour_bucket: String,
    pub(super) hour_ts: UtcTime,
    pub(super) quantity: i64,
    pub(super) row_keys: Vec<String>,
    pub(super) allocations: Vec<Allocation>,
    pub(super) tagged_nonzero_rows: usize,
    pub(super) untagged_nonzero_rows: usize,
    pub(super) reject_reason: Option<&'static str>,
    pub(super) customer_identifier: String,
}

/// `merge_allocations`: one row's seller-provided allocations into the group's
/// tag sets, summing identical sets. A reason code when malformed (the sets
/// merged before the fault stay, as in the script).
pub(super) fn merge_allocations(
    entries: &mut Vec<Allocation>,
    row_allocs: &Json,
) -> Option<&'static str> {
    let Some(list) = list_value(row_allocs) else {
        return Some("MalformedAllocations");
    };
    for alloc in list {
        if object_members(alloc).is_none() {
            return Some("MalformedAllocations");
        }
        let allocated = alloc.get("AllocatedUsageQuantity").and_then(strict_int);
        let Some(allocated) = allocated.filter(|quantity| *quantity >= 0) else {
            return Some("NegativeOrNonIntegerAllocation");
        };
        let empty = Json::Array(Vec::new());
        let tags_value = alloc.get("Tags").unwrap_or(&empty);
        let Some(tags) = list_value(tags_value) else {
            return Some("MissingTagKeyOrValue");
        };
        if tags.len() > MAX_TAGS_PER_ALLOCATION {
            return Some("TooManyTags");
        }
        if tags.is_empty() {
            return Some("MissingTagKeyOrValue");
        }
        let mut normalized = Vec::with_capacity(tags.len());
        for tag in tags {
            let key = tag.get("Key").filter(|value| truthy(value));
            let value = tag.get("Value").filter(|value| truthy(value));
            match (object_members(tag), key, value) {
                (Some(_), Some(key), Some(value)) => normalized.push((text(key), text(value))),
                _ => return Some("MissingTagKeyOrValue"),
            }
        }
        let mut signature = normalized.clone();
        signature.sort();
        match entries
            .iter_mut()
            .find(|entry| entry.signature == signature)
        {
            Some(entry) => match entry.qty.checked_add(allocated) {
                Some(sum) => entry.qty = sum,
                // The script's exact sum has no 64-bit form; clamping it would
                // let it pass for a legitimate quantity.
                None => return Some("QuantityOutOfRange"),
            },
            None => entries.push(Allocation {
                signature,
                tags: normalized,
                qty: allocated,
            }),
        }
    }
    None
}

/// The fold of `collect_group` over already-read rows.
pub(super) fn fold_rows(
    license_arn: &str,
    account_id: &str,
    dimension: &str,
    bucket: &str,
    rows: &[Json],
) -> Result<Group, Failure> {
    let hour_ts = parse_hour_bucket(bucket)
        .ok_or_else(|| Failure::message("hourBucket must look like 2026-01-31T12"))?;
    let mut group = Group {
        license_arn: license_arn.to_string(),
        account_id: account_id.to_string(),
        dimension: dimension.to_string(),
        hour_bucket: bucket.to_string(),
        hour_ts,
        quantity: 0,
        row_keys: Vec::new(),
        allocations: Vec::new(),
        tagged_nonzero_rows: 0,
        untagged_nonzero_rows: 0,
        reject_reason: None,
        customer_identifier: String::new(),
    };
    for row in rows {
        if row.get("meteringPending").is_none() {
            continue;
        }
        let sort_key = ddb::text_or_empty(row, "customerAWSAccountId_dimension_timestamp")?;
        group.row_keys.push(sort_key.clone());
        if let Some(reason) = validate_sort_key(&sort_key, account_id, dimension) {
            group.reject_reason = group.reject_reason.or(Some(reason));
            continue;
        }
        if group.customer_identifier.is_empty() {
            group.customer_identifier = ddb::text_or_empty(row, "customerIdentifier")?;
        }
        let zero = Json::Int(0);
        let raw = row.get("quantity").unwrap_or(&zero);
        let Some(quantity) = strict_int(raw) else {
            group.reject_reason = group.reject_reason.or(Some(quantity_reason(raw)));
            continue;
        };
        if quantity < 0 {
            group.reject_reason = group.reject_reason.or(Some("NegativeQuantity"));
            continue;
        }
        let Some(sum) = group.quantity.checked_add(quantity) else {
            group.reject_reason = group.reject_reason.or(Some("QuantityOutOfRange"));
            continue;
        };
        group.quantity = sum;
        if quantity == 0 {
            continue;
        }
        match row.get("usageAllocations").filter(|value| truthy(value)) {
            Some(allocations) => {
                group.tagged_nonzero_rows += 1;
                if let Some(reason) = merge_allocations(&mut group.allocations, allocations) {
                    group.reject_reason = group.reject_reason.or(Some(reason));
                }
            }
            None => group.untagged_nonzero_rows += 1,
        }
    }
    Ok(group)
}

/// `^arn:aws[a-zA-Z-]*:license-manager:[^:]*:[0-9]{12}:license:.+$` with
/// Python's `$` (also before one trailing newline) and `.` (no newline).
fn is_license_arn(arn: &str) -> bool {
    let Some(rest) = arn.strip_prefix("arn:aws") else {
        return false;
    };
    let rest = rest.trim_start_matches(|c: char| c.is_ascii_alphabetic() || c == '-');
    let Some(rest) = rest.strip_prefix(":license-manager:") else {
        return false;
    };
    let Some((_region, rest)) = rest.split_once(':') else {
        return false;
    };
    if rest.len() < 13 || !rest.is_char_boundary(12) {
        return false;
    }
    let (account, rest) = rest.split_at(12);
    if !account.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let Some(tail) = rest.strip_prefix(":license:") else {
        return false;
    };
    let body = tail.strip_suffix('\n').unwrap_or(tail);
    !body.is_empty() && !body.contains('\n')
}

/// `validate_group`: the reason a folded group is invalid, else `None`.
pub(super) fn validate_group(group: &Group, now: UtcTime) -> Option<&'static str> {
    if let Some(reason) = group.reject_reason {
        return Some(reason);
    }
    if group.dimension.is_empty() {
        return Some("MissingDimension");
    }
    if group.hour_ts > now {
        return Some("TimestampInFuture");
    }
    if group.hour_ts < add_hours(now, -24) {
        return Some("TimestampOutOfWindow");
    }
    if group.license_arn.starts_with("arn:") {
        if !is_license_arn(&group.license_arn) {
            return Some("InvalidLicenseArn");
        }
        if group.account_id.is_empty() {
            return Some("MissingCustomerAWSAccountId");
        }
    } else if group.customer_identifier.is_empty() && group.account_id.is_empty() {
        return Some("MissingLegacyIdentifier");
    }
    if group.tagged_nonzero_rows > 0 && group.untagged_nonzero_rows > 0 {
        return Some("MixedAllocatedAndUnallocated");
    }
    if !group.allocations.is_empty() {
        if group.allocations.len() > MAX_ALLOCATIONS_PER_RECORD {
            return Some("TooManyAllocations");
        }
        // The group quantity is a 64-bit sum, so an allocation total beyond
        // 64 bits is, as in the script's exact arithmetic, larger than it.
        let total = group
            .allocations
            .iter()
            .try_fold(0_i64, |sum, entry| sum.checked_add(entry.qty));
        if total != Some(group.quantity) {
            return Some("AllocationSumMismatch");
        }
    }
    None
}

/// `finalize_allocations`: the merged `UsageAllocations` list, empty to omit it.
pub(super) fn finalize_allocations(group: &Group) -> Vec<Json> {
    if group.untagged_nonzero_rows > 0 || group.allocations.is_empty() {
        return Vec::new();
    }
    group
        .allocations
        .iter()
        .map(|entry| {
            let tags = entry
                .tags
                .iter()
                .map(|(key, value)| {
                    Json::object(vec![
                        ("Key", Json::string(key.clone())),
                        ("Value", Json::string(value.clone())),
                    ])
                })
                .collect();
            Json::object(vec![
                ("AllocatedUsageQuantity", Json::Int(entry.qty)),
                ("Tags", Json::Array(tags)),
            ])
        })
        .collect()
}

/// `agg_sort_key`: `account#dimension#hour`.
pub(super) fn agg_sort_key(account_id: &str, dimension: &str, bucket: &str) -> String {
    format!("{account_id}#{dimension}#{bucket}")
}

// ---- table reads -----------------------------------------------------------

/// One `(licenseArn, customerAWSAccountId, dimension)` pending group.
pub(super) type PendingGroup = (String, String, String);

fn pending_groups(
    ctx: &mut Ctx<'_>,
    bucket: &str,
    license: Option<&str>,
) -> Result<Vec<PendingGroup>, Failure> {
    let table = ctx.usage_table()?.to_string();
    let mut values = vec![(":hb", Json::string(bucket))];
    let condition = if let Some(license) = license {
        values.push((":la", Json::string(license)));
        "meteringPending = :hb AND licenseArn = :la"
    } else {
        "meteringPending = :hb"
    };
    let query = Query::new(&table, Some("metering_pending"), condition, values);
    let mut seen: Vec<PendingGroup> = Vec::new();
    for item in query_all(ctx.client, &query)? {
        let arn = ddb::text_or_empty(&item, "licenseArn")?;
        if arn.is_empty() {
            continue;
        }
        let group = (
            arn,
            ddb::text_or_empty(&item, "customerAWSAccountId")?,
            ddb::text_or_empty(&item, "dimension")?,
        );
        if !seen.contains(&group) {
            seen.push(group);
        }
    }
    Ok(seen)
}

/// `get_pending_groups`: the de-duplicated pending groups of an hour bucket.
pub(super) fn get_pending_groups(
    ctx: &mut Ctx<'_>,
    bucket: &str,
) -> Result<Vec<PendingGroup>, Failure> {
    pending_groups(ctx, bucket, None)
}

/// `get_pending_groups_for_license`: the same read scoped to one license.
pub(super) fn get_pending_groups_for_license(
    ctx: &mut Ctx<'_>,
    bucket: &str,
    license_arn: &str,
) -> Result<Vec<PendingGroup>, Failure> {
    pending_groups(ctx, bucket, Some(license_arn))
}

/// `read_group_rows`: the rows of one group by sort-key prefix.
pub(super) fn read_group_rows(
    ctx: &mut Ctx<'_>,
    license_arn: &str,
    account_id: &str,
    dimension: &str,
    bucket: &str,
) -> Result<Vec<Json>, Failure> {
    let table = ctx.usage_table()?.to_string();
    let prefix = format!("{account_id}#{dimension}#{bucket}");
    let query = Query::new(
        &table,
        None,
        "licenseArn = :la AND begins_with(customerAWSAccountId_dimension_timestamp, :pfx)",
        vec![
            (":la", Json::string(license_arn)),
            (":pfx", Json::string(prefix)),
        ],
    );
    query_all(ctx.client, &query)
}

/// `collect_group`.
pub(super) fn collect_group(
    ctx: &mut Ctx<'_>,
    license_arn: &str,
    account_id: &str,
    dimension: &str,
    bucket: &str,
) -> Result<Group, Failure> {
    let rows = read_group_rows(ctx, license_arn, account_id, dimension, bucket)?;
    fold_rows(license_arn, account_id, dimension, bucket, &rows)
}

/// The licenses in a deprovisioning flush window with their expiry (the index
/// RANGE key), in query order.
pub(super) fn deprovisioning_licenses(
    ctx: &mut Ctx<'_>,
) -> Result<Vec<(String, Option<String>)>, Failure> {
    let table = ctx.subscribers_table()?.to_string();
    let query = Query::new(
        &table,
        Some(DEPROVISIONING_PENDING_INDEX),
        "deprovisioningPendingFlag = :v",
        vec![(":v", Json::string(DEPROVISIONING_PENDING_VALUE))],
    );
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    for item in query_all(ctx.client, &query)? {
        let arn = ddb::text_or_empty(&item, "licenseArn")?;
        if arn.is_empty() {
            continue;
        }
        let expiry = item
            .get("deprovisioningExpiry")
            .and_then(Json::as_str)
            .map(str::to_string);
        match out.iter_mut().find(|(known, _)| *known == arn) {
            Some(slot) => slot.1 = expiry,
            None => out.push((arn, expiry)),
        }
    }
    Ok(out)
}

// ---- metrics ---------------------------------------------------------------

/// `emf_metric`: a CloudWatch Embedded Metric Format line on the log.
///
/// `UsageRecordRejected` with a reason and `BatchMeterUsageException` with an
/// exception carry their own dimension and `MeteringMode`; the rest are plain
/// `[ProductCode]` counters.
///
/// The product code, mode, namespace, reason and exception come from the
/// configuration or from a service answer, so each goes through `shown`
/// (redacted, bounded, escaped) before it is serialized; the line stays one
/// parseable JSON object.
pub(super) fn emf_metric(
    ctx: &mut Ctx<'_>,
    name: &str,
    value: i64,
    reason: Option<&str>,
    exception: Option<&str>,
) {
    let product = Json::string(shown(ctx.product_code()));
    let mode = Json::string(shown(&ctx.metering_mode()));
    let (dimensions, mut fields): (Vec<&str>, Vec<(&str, Json)>) = match (name, reason, exception) {
        ("UsageRecordRejected", Some(reason), _) if !reason.is_empty() => (
            vec!["Reason", "MeteringMode", "ProductCode"],
            vec![
                ("Reason", Json::string(shown(reason))),
                ("ProductCode", product),
                ("MeteringMode", mode),
            ],
        ),
        ("BatchMeterUsageException", _, Some(exception)) if !exception.is_empty() => (
            vec!["Exception", "MeteringMode", "ProductCode"],
            vec![
                ("Exception", Json::string(shown(exception))),
                ("ProductCode", product),
                ("MeteringMode", mode),
            ],
        ),
        _ => (vec!["ProductCode"], vec![("ProductCode", product)]),
    };
    fields.push((name, Json::Int(value)));
    let metrics = Json::object(vec![
        ("Name", Json::string(name)),
        ("Unit", Json::string("Count")),
    ]);
    let directive = Json::object(vec![
        ("Namespace", Json::string(shown(ctx.metric_namespace()))),
        (
            "Dimensions",
            Json::Array(vec![Json::Array(
                dimensions.into_iter().map(Json::string).collect(),
            )]),
        ),
        ("Metrics", Json::Array(vec![metrics])),
    ]);
    let aws = Json::object(vec![
        ("Timestamp", Json::Int(epoch(ctx.now) * 1000)),
        ("CloudWatchMetrics", Json::Array(vec![directive])),
    ]);
    let mut members = vec![("_aws", aws)];
    members.extend(fields);
    ctx.log.metric(dumps(&Json::object(members), false));
}

// ---- bounded writes ----------------------------------------------------------

const THROTTLE_CODES: [&str; 3] = [
    "ProvisionedThroughputExceededException",
    "ThrottlingException",
    "RequestLimitExceeded",
];

/// One raw-usage-row `UpdateItem`, retried on per-partition throttling up to
/// `CLEANUP_MAX_RETRIES` times. Any other failure, or exhausted retries, is
/// returned so the caller fails the message (redelivery, DLQ). The jittered
/// back-off sleep of the script is not performed: this build sends nothing.
pub(super) fn update_item_with_backoff(
    ctx: &mut Ctx<'_>,
    license_arn: &str,
    sort_key: &str,
    expression: &str,
    values: Vec<(String, Json)>,
) -> Result<(), Failure> {
    let table = ctx.usage_table()?.to_string();
    let max_retries = ctx.config.integer("CLEANUP_MAX_RETRIES", 5)?;
    let (expression, values) = with_audit_timestamps(expression, values, ctx.now);
    let update = Update {
        table: &table,
        key: vec![
            ("licenseArn", Json::string(license_arn)),
            (
                "customerAWSAccountId_dimension_timestamp",
                Json::string(sort_key),
            ),
        ],
        expression: &expression,
        condition: None,
        values: Some(values),
        names: None,
    };
    let mut attempt = 0_i64;
    loop {
        match ddb::update_item(ctx.client, &update) {
            Ok(()) => return Ok(()),
            Err(error)
                if attempt < max_retries
                    && service_code(&error).is_some_and(|code| THROTTLE_CODES.contains(&code)) =>
            {
                attempt += 1;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// `update_rows_bounded`: one update per non-empty sort key of a license.
/// Every row is tried; the failures are counted and reported together, never
/// dropped silently.
pub(super) fn update_rows_bounded(
    ctx: &mut Ctx<'_>,
    license_arn: &str,
    sort_keys: &[String],
    expression: &str,
    values: &[(String, Json)],
) -> Result<(), Failure> {
    let mut failures = 0_usize;
    let mut first: Option<String> = None;
    for sort_key in sort_keys.iter().filter(|key| !key.is_empty()) {
        if let Err(error) =
            update_item_with_backoff(ctx, license_arn, sort_key, expression, values.to_vec())
        {
            failures += 1;
            if first.is_none() {
                first = Some(match &error {
                    Failure::Aws(aws) => aws_class(aws),
                    other @ Failure::Message(_) => other.to_string(),
                });
            }
        }
    }
    match first {
        None => Ok(()),
        Some(class) => Err(Failure::message(format!(
            "{failures}/{} UpdateItem(s) failed for license={}; first error: {}",
            sort_keys.len(),
            shown(&mask(license_arn)),
            class
        ))),
    }
}

// ---- BatchMeterUsage submit --------------------------------------------------

/// The terminal result of one aggregated record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Outcome {
    pub(super) metering_record_id: Option<String>,
    pub(super) status: String,
    pub(super) reason: Option<String>,
}

/// The `(licenseArn, account_dimension_hour)` key of an aggregated record.
pub(super) type AggKey = (String, String);

/// Outcomes in insertion order; a later outcome of a key replaces the earlier
/// one in place (a Python `dict`).
#[derive(Debug, Default)]
pub(super) struct Outcomes {
    entries: Vec<(AggKey, Outcome)>,
}

impl Outcomes {
    pub(super) fn set(&mut self, key: AggKey, outcome: Outcome) {
        match self.entries.iter_mut().find(|(known, _)| *known == key) {
            Some(slot) => slot.1 = outcome,
            None => self.entries.push((key, outcome)),
        }
    }

    pub(super) fn entries(&self) -> &[(AggKey, Outcome)] {
        &self.entries
    }
}

fn is_transient(code: &str) -> bool {
    matches!(
        code,
        "ThrottlingException"
            | "InternalServiceException"
            | "ServiceUnavailable"
            | "DisabledApiException"
    )
}

/// The `(license or customer, account, dimension, hour)` identity of a usage record.
type Identity = (String, String, String, String);

/// `_ts_iso` of a record timestamp: an epoch number is an hour bucket in UTC,
/// a text keeps its first 13 characters.
fn ts_iso(value: Option<&Json>) -> String {
    match value {
        Some(Json::Int(seconds)) => bucket_of_epoch(*seconds),
        #[allow(clippy::cast_possible_truncation)]
        Some(Json::Float(seconds)) => bucket_of_epoch(*seconds as i64),
        Some(other) => text(other).chars().take(13).collect(),
        None => "None".to_string(),
    }
}

fn bucket_of_epoch(seconds: i64) -> String {
    UtcTime::from_epoch_micros(seconds.saturating_mul(1_000_000))
        .map_or_else(|| seconds.to_string(), hour_bucket)
}

fn identity(record: &Json) -> Result<Identity, Failure> {
    let license = ddb::text_or_empty(record, "LicenseArn")?;
    let first = if license.is_empty() {
        ddb::text_or_empty(record, "CustomerIdentifier")?
    } else {
        license
    };
    Ok((
        first,
        ddb::text_or_empty(record, "CustomerAWSAccountId")?,
        ddb::text_or_empty(record, "Dimension")?,
        ts_iso(record.get("Timestamp")),
    ))
}

/// `submit_records`: sends one batch of at most 25 records, captures each
/// record's terminal status into `outcomes`, retries unprocessed records once
/// and isolates a request-level failure by bisecting (bounded). Returns `true`
/// when records remain for the next run. Only the recorded or no-live client
/// answers: this build never reaches `BatchMeterUsage`.
pub(super) fn submit_records(
    ctx: &mut Ctx<'_>,
    records: &[Json],
    keys: &[AggKey],
    outcomes: &mut Outcomes,
    use_product_code: bool,
) -> Result<bool, Failure> {
    if records.is_empty() {
        return Ok(false);
    }
    submit(ctx, records, keys, outcomes, use_product_code, 0)
}

fn batch_request(ctx: &Ctx<'_>, records: &[Json], use_product_code: bool) -> Json {
    let mut members = vec![("UsageRecords", Json::Array(records.to_vec()))];
    if use_product_code {
        members.push(("ProductCode", Json::string(ctx.product_code())));
    }
    Json::object(members)
}

fn submit(
    ctx: &mut Ctx<'_>,
    records: &[Json],
    keys: &[AggKey],
    outcomes: &mut Outcomes,
    use_product_code: bool,
    depth: u32,
) -> Result<bool, Failure> {
    let mut by_identity: Vec<(Identity, AggKey)> = Vec::new();
    for (record, key) in records.iter().zip(keys) {
        let id = identity(record)?;
        match by_identity.iter_mut().find(|(known, _)| *known == id) {
            Some(slot) => slot.1 = key.clone(),
            None => by_identity.push((id, key.clone())),
        }
    }
    let request = batch_request(ctx, records, use_product_code);
    let response = match ctx
        .client
        .call(Service::Metering, "BatchMeterUsage", &request)
    {
        Ok(response) => response,
        Err(error) => {
            let Some(code) = service_code(&error).map(str::to_string) else {
                return Err(error.into());
            };
            if is_transient(&code) {
                return Err(error.into());
            }
            return isolate(ctx, records, keys, outcomes, use_product_code, depth, &code);
        }
    };
    capture_results(ctx, &response, &by_identity, outcomes)?;
    let unprocessed = unprocessed_records(&response)?;
    if unprocessed.is_empty() {
        return Ok(false);
    }
    ctx.log.warning(&format!(
        "{} unprocessed record(s); retrying once",
        unprocessed.len()
    ));
    let retry_request = batch_request(ctx, &unprocessed, use_product_code);
    let retry = ctx
        .client
        .call(Service::Metering, "BatchMeterUsage", &retry_request)?;
    capture_results(ctx, &retry, &by_identity, outcomes)?;
    let still = unprocessed_records(&retry)?;
    if still.is_empty() {
        return Ok(false);
    }
    ctx.log.error(&format!(
        "{} record(s) still unprocessed after retry; left pending for the next run",
        still.len()
    ));
    emf_metric(
        ctx,
        "UsageRecordUnprocessed",
        i64::try_from(still.len()).unwrap_or(i64::MAX),
        None,
        None,
    );
    Ok(true)
}

fn unprocessed_records(response: &Json) -> Result<Vec<Json>, Failure> {
    match toolkit_aws::recorded::nullable_list(response, "UnprocessedRecords") {
        Ok(list) => Ok(list.unwrap_or_default().to_vec()),
        Err(error) => Err(Failure::message(format!(
            "invalid BatchMeterUsage response: {error}"
        ))),
    }
}

/// A request-level failure of a batch: the record is stamped `Error` when it
/// is alone, else the batch is halved until the depth cap.
fn isolate(
    ctx: &mut Ctx<'_>,
    records: &[Json],
    keys: &[AggKey],
    outcomes: &mut Outcomes,
    use_product_code: bool,
    depth: u32,
    code: &str,
) -> Result<bool, Failure> {
    if records.len() == 1 {
        let exception = if code.is_empty() {
            "RequestRejected"
        } else {
            code
        };
        outcomes.set(
            keys[0].clone(),
            Outcome {
                metering_record_id: None,
                status: STATUS_ERROR.to_string(),
                reason: Some(exception.to_string()),
            },
        );
        emf_metric(ctx, "BatchMeterUsageException", 1, None, Some(exception));
        ctx.log.warning(&format!(
            "Isolated request-level BatchMeterUsage exception ({}) for a single record",
            shown(exception)
        ));
        return Ok(false);
    }
    if depth >= MAX_BISECT_DEPTH {
        ctx.log.warning(&format!(
            "Bisect depth cap ({MAX_BISECT_DEPTH}) reached with {} record(s) still failing ({}); \
             leaving them pending for the next run rather than bisecting further",
            records.len(),
            shown(code)
        ));
        emf_metric(
            ctx,
            "BisectDepthExceeded",
            i64::try_from(records.len()).unwrap_or(i64::MAX),
            None,
            None,
        );
        return Ok(true);
    }
    let mid = records.len() / 2;
    let left = submit(
        ctx,
        &records[..mid],
        &keys[..mid],
        outcomes,
        use_product_code,
        depth + 1,
    )?;
    let right = submit(
        ctx,
        &records[mid..],
        &keys[mid..],
        outcomes,
        use_product_code,
        depth + 1,
    )?;
    Ok(left || right)
}

fn capture_results(
    ctx: &mut Ctx<'_>,
    response: &Json,
    by_identity: &[(Identity, AggKey)],
    outcomes: &mut Outcomes,
) -> Result<(), Failure> {
    let invalid = |error: toolkit_aws::recorded::MemberError| {
        Failure::message(format!("invalid BatchMeterUsage response: {error}"))
    };
    let empty = Json::Object(Vec::new());
    for result in
        toolkit_aws::recorded::optional_object_list(response, "Results").map_err(invalid)?
    {
        let record = toolkit_aws::recorded::optional_object(result, "UsageRecord")
            .map_err(invalid)?
            .unwrap_or(&empty);
        let status = optional_text(result, "Status")
            .map_err(invalid)?
            .unwrap_or("")
            .to_string();
        let id = identity(record)?;
        if let Some((_, key)) = by_identity.iter().find(|(known, _)| *known == id) {
            let record_id = toolkit_aws::recorded::nullable_text(result, "MeteringRecordId")
                .map_err(invalid)?
                .map(str::to_string);
            outcomes.set(
                key.clone(),
                Outcome {
                    metering_record_id: record_id,
                    status: status.clone(),
                    reason: None,
                },
            );
        }
        observe_status(ctx, &status, record)?;
    }
    Ok(())
}

/// `observe_status`: logs a non-success status and counts the two the
/// dashboards track.
fn observe_status(ctx: &mut Ctx<'_>, status: &str, record: &Json) -> Result<(), Failure> {
    if status == "Success" || status == "DuplicateRecord" {
        if status == "DuplicateRecord" {
            emf_metric(ctx, "DuplicateRecord", 1, None, None);
        }
        return Ok(());
    }
    let account = ddb::text_or_empty(record, "CustomerAWSAccountId")?;
    let dimension = optional_text(record, "Dimension")
        .map_err(|error| Failure::message(format!("invalid item: {error}")))?
        .unwrap_or("unknown")
        .to_string();
    ctx.log.warning(&format!(
        "Metering status={} account={} dimension={}",
        shown(status),
        shown(&mask(&account)),
        shown(&dimension)
    ));
    if status == "CustomerNotSubscribed" {
        emf_metric(ctx, "CustomerNotSubscribed", 1, None, None);
    }
    Ok(())
}
