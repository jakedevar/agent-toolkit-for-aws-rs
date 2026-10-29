//! `test_event_publisher.py`: EventBridge events in the shape of the AWS
//! Marketplace subscription events, with randomized but valid values, for a
//! non-production (`dry-run`) stage only. The events are a simulator's: they
//! confer no entitlement. Publishing goes through the client seam (recorded or
//! no-live), so this build never reaches EventBridge.

use std::fmt::Write as _;

use toolkit_aws::recorded::optional_member;
use toolkit_core::Json;

use super::base::{Ctx, Failure};
use super::client::Service;
use super::clock::stamp;
use super::pipeline::Handled;
use crate::cloudwatch::timefmt::UtcTime;
use crate::dynamodb::pyfmt::{dumps, truthy};

/// The reserved AWS service source prefix: only ever rejected.
pub(super) const RESERVED_SOURCE_PREFIX: &str = "aws.";

/// The scenarios of `--scenario`.
pub(super) const SCENARIOS: [&str; 10] = [
    "new",
    "deprovision",
    "agreement-ended",
    "agreement-amended",
    "multi-region",
    "multi-dimension",
    "burst",
    "malformed",
    "month-boundary",
    "duplicate",
];

const SAMPLE_DIMENSIONS: [&str; 4] = ["Requests", "GBHours", "Users", "DataProcessedGB"];

/// The detail-type a scenario publishes (the four the subscription Lambda
/// consumes; every compound scenario is a `License Updated` envelope).
fn detail_type(scenario: &str) -> &'static str {
    match scenario {
        "deprovision" => "License Deprovisioned",
        "agreement-ended" => "Purchase Agreement Ended",
        "agreement-amended" => "Purchase Agreement Amended",
        _ => "License Updated",
    }
}

/// A small deterministic generator (SplitMix64). The values it draws only
/// have to be well formed, never unpredictable: nothing here is a secret.
pub(super) struct Rng(u64);

impl Rng {
    pub(super) fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next_u64() % u64::try_from(bound.max(1)).unwrap_or(1)).unwrap_or(0)
    }

    /// `uuid.uuid4().hex[:length]`.
    fn hex(&mut self, length: usize) -> String {
        let mut text = String::new();
        while text.len() < length {
            let _ = write!(text, "{:016x}", self.next_u64());
        }
        text.truncate(length);
        text
    }
}

/// `_require_event_source`: a stage-scoped source is required and an `aws.*`
/// source is rejected.
pub(super) fn require_event_source(source: &str) -> Result<&str, String> {
    if source.is_empty() {
        return Err("event_source is required (a stage-scoped '<stageName>.agreement-marketplace'); there is no default".to_string());
    }
    if source.starts_with(RESERVED_SOURCE_PREFIX) {
        return Err("event_source uses the reserved 'aws.' prefix - use a stage-scoped '<stageName>.agreement-marketplace' instead".to_string());
    }
    Ok(source)
}

/// `build_event`: one event in AWS Marketplace shape for a scenario. The
/// acceptor is drawn only from the non-empty test-account allow-list.
pub(super) fn build_event(
    scenario: &str,
    test_accounts: &[String],
    product_code: &str,
    now: UtcTime,
    event_source: &str,
    rng: &mut Rng,
) -> Result<Json, String> {
    if test_accounts.is_empty() {
        return Err("test_accounts (TestAccountAllowlist) must be non-empty - non-prod test events may use test accounts ONLY".to_string());
    }
    let source = require_event_source(event_source)?;
    let account_id = test_accounts[rng.below(test_accounts.len())].clone();
    let license_arn = format!(
        "arn:aws:license-manager::{account_id}:license:l-{}",
        rng.hex(16)
    );
    let agreement_id = format!("agmt-{}", rng.hex(20));
    let end_time = now
        .checked_add_micros(365 * 24 * 3_600 * 1_000_000)
        .map_or_else(|| stamp(now), stamp);

    let kind = detail_type(scenario);
    let mut acceptor = vec![("accountId", Json::string(account_id))];
    let mut license = vec![("arn", Json::string(license_arn))];
    let mut detail_members = Vec::new();
    let mut entitlement: Option<Json> = None;
    if scenario == "deprovision" {
        entitlement = Some(Json::object(vec![(
            "status",
            Json::string("deprovisioning"),
        )]));
    } else if kind == "License Updated" {
        let dimension = SAMPLE_DIMENSIONS[rng.below(SAMPLE_DIMENSIONS.len())];
        entitlement = Some(Json::object(vec![
            ("status", Json::string("active")),
            ("dimension", Json::string(dimension)),
        ]));
    }
    if scenario == "malformed" {
        // A genuinely malformed License Updated: the required license.arn is
        // dropped and the account id is not twelve digits.
        license.clear();
        acceptor = vec![("accountId", Json::string("not-an-account-id"))];
    }
    detail_members.push(("acceptor", Json::object(acceptor)));
    detail_members.push(("license", Json::object(license)));
    detail_members.push((
        "agreement",
        Json::object(vec![
            ("id", Json::string(agreement_id)),
            ("endTime", Json::string(end_time)),
        ]),
    ));
    detail_members.push((
        "product",
        Json::object(vec![("code", Json::string(product_code))]),
    ));
    if let Some(entitlement) = entitlement {
        detail_members.push(("entitlement", entitlement));
    }
    Ok(Json::object(vec![
        ("Source", Json::string(source)),
        ("DetailType", Json::string(kind)),
        (
            "Detail",
            Json::string(dumps(&Json::object(detail_members), false)),
        ),
        ("scenario", Json::string(scenario)),
        ("time", Json::string(stamp(now))),
    ]))
}

/// `build_batch`: `count` events; `burst` implies 50 when `count` is 1;
/// `duplicate` repeats the first event once.
pub(super) fn build_batch(
    scenario: &str,
    test_accounts: &[String],
    product_code: &str,
    count: i64,
    now: UtcTime,
    event_source: &str,
    rng: &mut Rng,
) -> Result<Vec<Json>, String> {
    let count = if scenario == "burst" && count == 1 {
        50
    } else {
        count
    };
    let mut events = Vec::new();
    for _ in 0..count.max(0) {
        events.push(build_event(
            scenario,
            test_accounts,
            product_code,
            now,
            event_source,
            rng,
        )?);
    }
    if scenario == "duplicate" {
        if let Some(first) = events.first().cloned() {
            events.push(first);
        }
    }
    Ok(events)
}

/// `_publish`: `PutEvents` in chunks of ten; a failed entry is an error.
pub(super) fn publish(
    ctx: &mut Ctx<'_>,
    events: &[Json],
    bus_name: Option<&str>,
) -> Result<usize, Failure> {
    let mut sent = 0;
    for chunk in events.chunks(10) {
        let entries: Vec<Json> = chunk
            .iter()
            .map(|event| {
                let mut members = vec![
                    ("Source", event.get("Source").cloned().unwrap_or(Json::Null)),
                    (
                        "DetailType",
                        event.get("DetailType").cloned().unwrap_or(Json::Null),
                    ),
                    ("Detail", event.get("Detail").cloned().unwrap_or(Json::Null)),
                ];
                if let Some(bus) = bus_name {
                    members.push(("EventBusName", Json::string(bus)));
                }
                Json::object(members)
            })
            .collect();
        let request = Json::object(vec![("Entries", Json::Array(entries))]);
        let response = ctx.client.call(Service::Events, "PutEvents", &request)?;
        let failed =
            optional_member(
                &response,
                "FailedEntryCount",
                "an integer",
                |value| match value {
                    Json::Int(count) => Some(*count),
                    _ => None,
                },
            )
            .map_err(|error| Failure::message(format!("invalid PutEvents response: {error}")))?
            .unwrap_or(0);
        if failed != 0 {
            return Err(Failure::message(format!(
                "PutEvents reported {failed} failed entr(ies)"
            )));
        }
        sent += chunk.len();
    }
    Ok(sent)
}

/// `test_event_publisher.handler`: generate and publish per the invocation
/// payload. It runs only in a `dry-run` stage; `productCode` is per
/// invocation and has no default.
pub(super) fn test_event_publisher(ctx: &mut Ctx<'_>, event: &Json, seed: u64) -> Handled {
    if ctx.metering_mode() != "dry-run" {
        return Err(Failure::message(
            "test_event_publisher may run ONLY in a dry-run (non-prod) stage",
        ));
    }
    let accounts = ctx.config.list("TEST_ACCOUNT_ALLOWLIST");
    let product_code = event
        .get("productCode")
        .filter(|value| truthy(value))
        .and_then(Json::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            Failure::message(
                "productCode is required in the invocation payload, e.g. \
                 {\"scenario\": \"new\", \"productCode\": \"<awsMarketplaceProductCode>\"}",
            )
        })?;
    let source = ctx.config.text("EVENT_SOURCE").trim().to_string();
    require_event_source(&source).map_err(Failure::Message)?;
    let scenario = event
        .get("scenario")
        .and_then(Json::as_str)
        .unwrap_or("new")
        .to_string();
    let count = match event.get("count") {
        None => 1,
        Some(Json::Int(count)) => *count,
        Some(Json::String(text)) => {
            super::base::py_int(text).ok_or_else(|| Failure::message("count must be an integer"))?
        }
        Some(_) => return Err(Failure::message("count must be an integer")),
    };
    let mut rng = Rng::new(seed);
    let events = build_batch(
        &scenario,
        &accounts,
        &product_code,
        count,
        ctx.now,
        &source,
        &mut rng,
    )
    .map_err(Failure::Message)?;
    let bus = ctx.config.text("EVENT_BUS_NAME").to_string();
    let published = publish(ctx, &events, (!bus.is_empty()).then_some(bus.as_str()))?;
    Ok(Json::object(vec![
        ("scenario", Json::string(scenario)),
        ("productCode", Json::string(product_code)),
        (
            "published",
            Json::Int(i64::try_from(published).unwrap_or(i64::MAX)),
        ),
    ]))
}

/// `json.dumps(value, indent=2)`: two-space layout with non-ASCII text
/// written as `\uXXXX` (`ensure_ascii`).
pub(super) fn dumps_indented_ascii(value: &Json) -> String {
    let mut out = String::new();
    for c in value.to_pretty_string().chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0_u16; 2];
            for unit in c.encode_utf16(&mut units) {
                let _ = write!(out, "\\u{unit:04x}");
            }
        }
    }
    out
}
