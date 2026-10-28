//! The subscriber Lambdas: `subscription.py`, `deprovision_cleanup.py` and
//! `register.py`.

use std::fmt::Write as _;

use toolkit_aws::recorded::{optional_object_list, optional_text};
use toolkit_core::{parse_json, Json};

use super::base::{aws_class, service_code, shown, Ctx, Failure};
use super::client::Service;
use super::clock::stamp;
use super::core::{DEPROVISIONING_PENDING_INDEX, DEPROVISIONING_PENDING_VALUE};
use super::ddb::{self, query_all, Query, Update};
use super::pipeline::Handled;
use super::web::{b64decode, form_values, parse_qs};
use crate::cloudwatch::timefmt::parse_iso_timestamp;
use crate::dynamodb::pyfmt::{dumps, text, truthy};

/// AWS Marketplace keeps a deprovisioned license meterable about an hour.
const FLUSH_WINDOW_MICROS: i64 = 3_600 * 1_000_000;
const MAX_TOKEN_LENGTH: usize = 16_384;

/// `_mask`: only the last four characters of an identifier.
fn mask_tail(value: &str) -> String {
    if value.is_empty() {
        return "<none>".to_string();
    }
    let chars: Vec<char> = value.chars().collect();
    if chars.len() > 4 {
        format!(
            "****{}",
            chars[chars.len() - 4..].iter().collect::<String>()
        )
    } else {
        "****".to_string()
    }
}

fn masked(value: &str) -> String {
    shown(&mask_tail(value))
}

fn invalid(error: impl std::fmt::Display) -> Failure {
    Failure::message(format!("invalid event: {error}"))
}

/// `_stamp`: the audit stamps appended to a pure `SET` expression.
fn stamped(expression: &str, values: Vec<(String, Json)>, now: &str) -> (String, Json) {
    stamped_plus(expression, values, Vec::new(), now)
}

/// [`stamped`] with more values after `:now` (for a clause added after the stamps).
fn stamped_plus(
    expression: &str,
    mut values: Vec<(String, Json)>,
    extra: Vec<(String, Json)>,
    now: &str,
) -> (String, Json) {
    values.push((":now".to_string(), Json::string(now)));
    values.extend(extra);
    (
        format!("{expression}, createdAt = if_not_exists(createdAt, :now), updatedAt = :now"),
        Json::Object(values),
    )
}

/// `detail.get(outer, {}).get(inner, "")`: an absent step is empty; a step of
/// another type is a failure of the message (the `AttributeError`).
fn nested(detail: &Json, outer: &str, inner: &str) -> Result<String, Failure> {
    match detail.get(outer) {
        None => Ok(String::new()),
        Some(object) if toolkit_aws::recorded::object_members(object).is_some() => {
            ddb::text_or_empty(object, inner)
        }
        Some(_) => Err(Failure::message(format!(
            "invalid event: detail.{outer} must be an object"
        ))),
    }
}

fn subscriber_key<'a>(license_arn: &str, account_id: &str) -> Vec<(&'a str, Json)> {
    vec![
        ("licenseArn", Json::string(license_arn)),
        ("customerAWSAccountId", Json::string(account_id)),
    ]
}

fn update(
    ctx: &mut Ctx<'_>,
    key: Vec<(&str, Json)>,
    expression: &str,
    condition: Option<&str>,
    values: Json,
) -> Result<(), toolkit_aws::AwsError> {
    let table = ctx.config.text("SUBSCRIBERS_TABLE").to_string();
    ddb::update_item(
        ctx.client,
        &Update {
            table: &table,
            key,
            expression,
            condition,
            values: Some(values),
            names: None,
        },
    )
}

// ---- subscription -------------------------------------------------------------

/// `subscription.handler`: EventBridge Marketplace events from SQS into the
/// subscribers table. Bad bodies are skipped, not retried; a failed event is a
/// batch item failure.
pub(super) fn subscription(ctx: &mut Ctx<'_>, event: &Json) -> Handled {
    ctx.config.require("SUBSCRIBERS_TABLE")?;
    let mut failures = Vec::new();
    let records = toolkit_aws::recorded::optional_list(event, "Records").map_err(invalid)?;
    for record in records {
        let Some(Ok(body)) = record.get("body").and_then(Json::as_str).map(parse_json) else {
            ctx.log
                .error("Invalid SQS message body, skipping (not retried)");
            continue;
        };
        if body.get("detail-type").is_some() {
            if let Err(failure) = process_event(ctx, &body) {
                ctx.log
                    .error(&format!("Failed to process record: {failure}"));
                failures.push(Json::object(vec![(
                    "itemIdentifier",
                    Json::string(ddb::text_or_empty(record, "messageId")?),
                )]));
            }
        } else {
            ctx.log.warning("Unknown message format, skipping");
        }
    }
    Ok(Json::object(vec![(
        "batchItemFailures",
        Json::Array(failures),
    )]))
}

fn process_event(ctx: &mut Ctx<'_>, event: &Json) -> Result<(), Failure> {
    let detail_type = ddb::text_or_empty(event, "detail-type")?;
    let empty = Json::Object(Vec::new());
    let detail = event.get("detail").unwrap_or(&empty);
    ctx.log
        .info(&format!("EventBridge event: {}", shown(&detail_type)));
    if detail_type.contains("License Updated") {
        license_updated(ctx, detail)
    } else if detail_type.contains("License Deprovisioned") {
        license_deprovisioned(ctx, detail, event.get("time"))
    } else if detail_type.contains("Purchase Agreement Ended") {
        agreement_ended(ctx, detail)
    } else if detail_type.contains("Purchase Agreement Amended") {
        agreement_amended(ctx, detail)
    } else {
        if detail_type.contains("Purchase Agreement Created") {
            ctx.log
                .info("Purchase Agreement Created received; not used by metering path (ignored)");
        }
        Ok(())
    }
}

fn license_updated(ctx: &mut Ctx<'_>, detail: &Json) -> Result<(), Failure> {
    let account_id = nested(detail, "acceptor", "accountId")?;
    let agreement_id = nested(detail, "agreement", "id")?;
    let license_arn = nested(detail, "license", "arn")?;
    let product_code = nested(detail, "product", "code")?;
    if account_id.is_empty() || license_arn.is_empty() {
        ctx.log
            .error("Missing acceptorAccountId or licenseArn in License Updated event");
        return Ok(());
    }
    let now = stamp(ctx.now);
    let values = || {
        Json::object(vec![
            (":pc", Json::string(product_code.clone())),
            (":aid", Json::string(agreement_id.clone())),
            (":active", Json::string("active")),
            (":now", Json::string(now.clone())),
        ])
    };
    // A late or duplicate `License Updated` must not resurrect a license that
    // was deprovisioned: subscriptionStatus moves to active only when absent or
    // already active. The agreement metadata is always refreshed.
    let first = update(
        ctx,
        subscriber_key(&license_arn, &account_id),
        "SET productCode = :pc, agreementId = :aid, agreementStatus = :active, \
         subscriptionStatus = :active, createdAt = if_not_exists(createdAt, :now), updatedAt = :now",
        Some("attribute_not_exists(subscriptionStatus) OR subscriptionStatus = :active"),
        values(),
    );
    match first {
        Ok(()) => {}
        Err(error) if service_code(&error) == Some("ConditionalCheckFailedException") => {
            update(
                ctx,
                subscriber_key(&license_arn, &account_id),
                "SET productCode = :pc, agreementId = :aid, agreementStatus = :active, \
                 createdAt = if_not_exists(createdAt, :now), updatedAt = :now",
                None,
                values(),
            )?;
            ctx.log.warning(&format!(
                "License Updated arrived for a non-active subscription (account={} licenseArn={}); \
                 refreshed agreement metadata but left subscriptionStatus unchanged (not reactivating a revoked license).",
                masked(&account_id),
                masked(&license_arn)
            ));
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    }
    ctx.log.info(&format!(
        "License updated (active): account={} licenseArn={}",
        masked(&account_id),
        masked(&license_arn)
    ));
    Ok(())
}

/// `_deprovisioning_expiry`: the event time (else now) plus the flush window.
fn deprovisioning_expiry(ctx: &Ctx<'_>, event_time: Option<&Json>) -> String {
    let base = event_time
        .filter(|value| truthy(value))
        .and_then(|value| parse_iso_timestamp(&text(value)).ok())
        .unwrap_or(ctx.now);
    base.checked_add_micros(FLUSH_WINDOW_MICROS)
        .map_or_else(|| stamp(base), stamp)
}

/// `_resolve_license_arn`: the `agreementId-index` lookup used when an event
/// omits `license.arn`. A missing row is `""`; any error propagates.
fn resolve_license_arn(ctx: &mut Ctx<'_>, agreement_id: &str) -> Result<String, Failure> {
    if agreement_id.is_empty() {
        return Ok(String::new());
    }
    let table = ctx.config.text("SUBSCRIBERS_TABLE").to_string();
    let query = Query::new(
        &table,
        Some("agreementId-index"),
        "agreementId = :agr",
        vec![(":agr", Json::string(agreement_id))],
    )
    .with_limit(1);
    let response = ctx
        .client
        .call(Service::DynamoDb, "Query", &query.first_page())?;
    let items = optional_object_list(&response, "Items")
        .map_err(|error| Failure::message(format!("invalid Query response: {error}")))?;
    match items.first() {
        Some(item) => ddb::text_or_empty(item, "licenseArn"),
        None => Ok(String::new()),
    }
}

/// The subscriber an agreement or deprovision event names, or `None` (logged)
/// when it cannot be resolved.
fn resolve_subscriber(
    ctx: &mut Ctx<'_>,
    detail: &Json,
    event_name: &str,
) -> Result<Option<(String, String)>, Failure> {
    let account_id = nested(detail, "acceptor", "accountId")?;
    let mut license_arn = nested(detail, "license", "arn")?;
    if license_arn.is_empty() {
        let agreement_id = nested(detail, "agreement", "id")?;
        license_arn = resolve_license_arn(ctx, &agreement_id)?;
    }
    if account_id.is_empty() || license_arn.is_empty() {
        ctx.log
            .error(&format!("Cannot resolve subscriber for {event_name} event"));
        return Ok(None);
    }
    Ok(Some((license_arn, account_id)))
}

/// `License Deprovisioned`: opens the final-usage flush window. Finalizing to
/// `inactive` is the time-based `deprovision-cleanup`'s job, never this one's.
fn license_deprovisioned(
    ctx: &mut Ctx<'_>,
    detail: &Json,
    event_time: Option<&Json>,
) -> Result<(), Failure> {
    let Some((license_arn, account_id)) = resolve_subscriber(ctx, detail, "License Deprovisioned")?
    else {
        return Ok(());
    };
    let expiry = deprovisioning_expiry(ctx, event_time);
    let (expression, values) = stamped(
        "SET subscriptionStatus = :s, deprovisioningPendingFlag = :f, deprovisioningExpiry = :exp",
        vec![
            (":s".to_string(), Json::string("deprovisioning")),
            (":f".to_string(), Json::string(DEPROVISIONING_PENDING_VALUE)),
            (":exp".to_string(), Json::string(expiry.clone())),
        ],
        &stamp(ctx.now),
    );
    update(
        ctx,
        subscriber_key(&license_arn, &account_id),
        &expression,
        None,
        values,
    )?;
    ctx.log.info(&format!(
        "License deprovisioned (final-usage flush window open until {}): account={} licenseArn={} \
         subscriptionStatus=deprovisioning",
        shown(&expiry),
        masked(&account_id),
        masked(&license_arn)
    ));
    Ok(())
}

/// `Purchase Agreement Ended`: `agreementStatus = inactive` only; metering and
/// `subscriptionStatus` are untouched.
fn agreement_ended(ctx: &mut Ctx<'_>, detail: &Json) -> Result<(), Failure> {
    let Some((license_arn, account_id)) =
        resolve_subscriber(ctx, detail, "Purchase Agreement Ended")?
    else {
        return Ok(());
    };
    let (expression, values) = stamped(
        "SET agreementStatus = :inactive",
        vec![(":inactive".to_string(), Json::string("inactive"))],
        &stamp(ctx.now),
    );
    update(
        ctx,
        subscriber_key(&license_arn, &account_id),
        &expression,
        None,
        values,
    )?;
    ctx.log.info(&format!(
        "Purchase Agreement Ended (agreement status only; metering unaffected): account={} \
         licenseArn={} agreementStatus=inactive",
        masked(&account_id),
        masked(&license_arn)
    ));
    Ok(())
}

/// `Purchase Agreement Amended`: the end time and `agreementStatus = active`.
fn agreement_amended(ctx: &mut Ctx<'_>, detail: &Json) -> Result<(), Failure> {
    let end_time = nested(detail, "agreement", "endTime")?;
    let Some((license_arn, account_id)) =
        resolve_subscriber(ctx, detail, "Purchase Agreement Amended")?
    else {
        return Ok(());
    };
    let (expression, values) = stamped(
        "SET endTime = :et, agreementStatus = :active",
        vec![
            (":et".to_string(), Json::string(end_time)),
            (":active".to_string(), Json::string("active")),
        ],
        &stamp(ctx.now),
    );
    update(
        ctx,
        subscriber_key(&license_arn, &account_id),
        &expression,
        None,
        values,
    )?;
    ctx.log.info(&format!(
        "Agreement amended: account={} licenseArn={}",
        masked(&account_id),
        masked(&license_arn)
    ));
    Ok(())
}

// ---- deprovision-cleanup ----------------------------------------------------------

/// `deprovision_cleanup.handler`: finalizes the licenses whose flush window
/// has elapsed (`deprovisioning` to `inactive`, both markers removed). The
/// update is conditioned on the marker still being present, so a duplicate run
/// or a re-deprovisioned row is safe.
pub(super) fn deprovision_cleanup(ctx: &mut Ctx<'_>, _event: &Json) -> Handled {
    let table = ctx.config.require("SUBSCRIBERS_TABLE")?.to_string();
    let now = stamp(ctx.now);
    let query = Query::new(
        &table,
        Some(DEPROVISIONING_PENDING_INDEX),
        "deprovisioningPendingFlag = :f AND deprovisioningExpiry <= :now",
        vec![
            (":f", Json::string(DEPROVISIONING_PENDING_VALUE)),
            (":now", Json::string(now.clone())),
        ],
    );
    let mut finalized = 0_i64;
    for item in query_all(ctx.client, &query)? {
        finalized += finalize(ctx, &item, &now)?;
    }
    ctx.log.info(&format!(
        "Deprovision cleanup: finalized {finalized} expired license(s) as inactive"
    ));
    Ok(Json::object(vec![("finalized", Json::Int(finalized))]))
}

fn finalize(ctx: &mut Ctx<'_>, item: &Json, now: &str) -> Result<i64, Failure> {
    let license_arn = ddb::text_or_empty(item, "licenseArn")?;
    let account_id = ddb::text_or_empty(item, "customerAWSAccountId")?;
    if license_arn.is_empty() || account_id.is_empty() {
        return Ok(0);
    }
    let values = Json::object(vec![
        (":inactive", Json::string("inactive")),
        (":now", Json::string(now)),
    ]);
    match update(
        ctx,
        subscriber_key(&license_arn, &account_id),
        "SET subscriptionStatus = :inactive, updatedAt = :now \
         REMOVE deprovisioningPendingFlag, deprovisioningExpiry",
        Some("attribute_exists(deprovisioningPendingFlag)"),
        values,
    ) {
        Ok(()) => {
            ctx.log.info(&format!(
                "Finalized deprovisioning->inactive for licenseArn={} (window elapsed)",
                masked(&license_arn)
            ));
            Ok(1)
        }
        Err(error) if service_code(&error) == Some("ConditionalCheckFailedException") => Ok(0),
        Err(error) => Err(error.into()),
    }
}

// ---- register -----------------------------------------------------------------------

fn http_response(status: i64, body: &Json) -> Json {
    Json::object(vec![
        ("statusCode", Json::Int(status)),
        (
            "headers",
            Json::object(vec![
                ("Content-Type", Json::string("application/json")),
                ("Cache-Control", Json::string("no-store")),
                ("X-Content-Type-Options", Json::string("nosniff")),
                ("X-Frame-Options", Json::string("DENY")),
                (
                    "Strict-Transport-Security",
                    Json::string("max-age=31536000; includeSubDomains"),
                ),
            ]),
        ),
        ("body", Json::string(dumps(body, false))),
    ])
}

#[allow(clippy::unnecessary_wraps)]
fn error_response(status: i64, message: &str) -> Handled {
    Ok(http_response(
        status,
        &Json::object(vec![("error", Json::string(message))]),
    ))
}

/// The request body of an API Gateway event: empty when absent or `null`,
/// base64-decoded when `isBase64Encoded` is truthy.
fn request_body(event: &Json) -> Result<String, Failure> {
    let body = match event.get("body") {
        None | Some(Json::Null) => String::new(),
        Some(Json::String(body)) => body.clone(),
        Some(_) => return Err(invalid("body must be a string")),
    };
    if event.get("isBase64Encoded").is_some_and(truthy) {
        let bytes = b64decode(&body)
            .map_err(|reason| Failure::message(format!("invalid base64 body ({reason})")))?;
        return String::from_utf8(bytes)
            .map_err(|_| Failure::message("the decoded body is not UTF-8"));
    }
    Ok(body)
}

/// `_extract_registration_fields`: only the allow-listed fields, each bounded
/// in length, up to the field-count bound; the rest is dropped, never stored.
fn registration_fields(
    ctx: &mut Ctx<'_>,
    form: &[(String, Vec<String>)],
) -> Result<Vec<(String, String)>, Failure> {
    let allowed = ctx.config.list("ALLOWED_REGISTRATION_FIELDS");
    let max_fields = ctx.config.integer("MAX_REGISTRATION_FIELDS", 20)?;
    let max_length =
        usize::try_from(ctx.config.integer("MAX_FIELD_VALUE_LENGTH", 512)?).unwrap_or(0);
    let mut data: Vec<(String, String)> = Vec::new();
    let mut count = 0_i64;
    for key in allowed {
        if count >= max_fields {
            break;
        }
        let Some(value) = form_values(form, &key).first() else {
            continue;
        };
        let mut value = value.clone();
        if value.chars().count() > max_length {
            ctx.log.warning(&format!(
                "Registration field '{}' exceeds max length; truncating",
                shown(&key)
            ));
            value = value.chars().take(max_length).collect();
        }
        match data.iter_mut().find(|(known, _)| *known == key) {
            Some(slot) => slot.1 = value,
            None => data.push((key, value)),
        }
        count += 1;
    }
    Ok(data)
}

/// `register.handler`: resolves the fulfillment token and upserts the buyer.
pub(super) fn register(ctx: &mut Ctx<'_>, event: &Json) -> Handled {
    ctx.config.require("SUBSCRIBERS_TABLE")?;
    let expected_product = ctx.config.require("PRODUCT_CODE")?.to_string();
    let body = request_body(event)?;
    if body.is_empty() || body.chars().count() > MAX_TOKEN_LENGTH {
        return error_response(400, "Invalid request body");
    }
    let form = parse_qs(&body);
    let token = form_values(&form, "x-amzn-marketplace-token")
        .first()
        .filter(|token| !token.trim().is_empty())
        .cloned();
    let Some(token) = token else {
        return error_response(400, "Missing x-amzn-marketplace-token");
    };
    let request = Json::object(vec![("RegistrationToken", Json::string(token))]);
    let result = match ctx
        .client
        .call(Service::Metering, "ResolveCustomer", &request)
    {
        Ok(result) => result,
        Err(error) => {
            return match service_code(&error) {
                Some("ExpiredTokenException") => error_response(
                    400,
                    "Registration token expired. Please return to AWS Marketplace and click Set up your account again.",
                ),
                Some("InvalidTokenException") => error_response(400, "Invalid registration token"),
                _ => {
                    ctx.log
                        .error(&format!("ResolveCustomer failed: {}", aws_class(&error)));
                    error_response(500, "Failed to resolve customer. Please try again.")
                }
            };
        }
    };
    let field = |name: &str| {
        optional_text(&result, name)
            .ok()
            .flatten()
            .unwrap_or("")
            .to_string()
    };
    let (account_id, license_arn, product_code) = (
        field("CustomerAWSAccountId"),
        field("LicenseArn"),
        field("ProductCode"),
    );
    if account_id.is_empty() || product_code.is_empty() {
        ctx.log
            .error("ResolveCustomer response missing required fields");
        return error_response(500, "Failed to resolve customer. Please try again.");
    }
    if product_code != expected_product {
        ctx.log.error(&format!(
            "Product code mismatch: received={} expected={}",
            shown(&product_code),
            shown(&expected_product)
        ));
        return error_response(400, "Product code mismatch");
    }
    if license_arn.is_empty() {
        ctx.log.error(&format!(
            "ResolveCustomer returned no LicenseArn for account={}; cannot persist a keyed \
             subscriber row (a License Updated event will create it)",
            masked(&account_id)
        ));
        return Ok(http_response(
            200,
            &Json::object(vec![(
                "message",
                Json::string("Registration received; your account is being activated."),
            )]),
        ));
    }

    let data = registration_fields(ctx, &form)?;
    let profile_table = ctx.config.text("CUSTOMER_PROFILE_TABLE").to_string();
    if profile_table.is_empty() {
        ctx.log
            .error("CUSTOMER_PROFILE_TABLE not configured; cannot persist buyer profile");
        return error_response(500, "Registration store not configured. Please try again.");
    }
    let now = stamp(ctx.now);
    upsert_profile(
        ctx,
        &profile_table,
        &license_arn,
        &account_id,
        &product_code,
        &data,
        &now,
    )?;
    upsert_subscriber(ctx, &license_arn, &account_id, &product_code, &now)?;
    let region = ctx.config.text("AWS_REGION").to_string();
    ctx.log.info(&format!(
        "Registered customer account={} region={} (PII -> in-region customer-profile table; subscribers row PII-free)",
        masked(&account_id),
        shown(&region)
    ));
    Ok(http_response(
        200,
        &Json::object(vec![(
            "message",
            Json::string("Registration successful. Your account is being activated."),
        )]),
    ))
}

/// The in-region profile row: `productCode` always, the registration data and
/// the promoted top-level fields when there is data. Index based expression
/// tokens keep a field name with a hyphen or a space out of the expression.
fn upsert_profile(
    ctx: &mut Ctx<'_>,
    table: &str,
    license_arn: &str,
    account_id: &str,
    product_code: &str,
    data: &[(String, String)],
    now: &str,
) -> Result<(), Failure> {
    let promoted = ctx.config.list("PROMOTED_PROFILE_FIELDS");
    let mut expression = "SET productCode = :pc".to_string();
    let mut values = vec![(":pc".to_string(), Json::string(product_code))];
    let mut names: Vec<(String, Json)> = Vec::new();
    if !data.is_empty() {
        expression.push_str(", registrationData = :rd");
        values.push((
            ":rd".to_string(),
            Json::Object(
                data.iter()
                    .map(|(key, value)| (key.clone(), Json::string(value.clone())))
                    .collect(),
            ),
        ));
        for (index, field) in promoted.iter().enumerate() {
            if let Some((_, value)) = data.iter().find(|(key, _)| key == field) {
                let _ = write!(expression, ", #n{index} = :v{index}");
                names.push((format!("#n{index}"), Json::string(field.clone())));
                values.push((format!(":v{index}"), Json::string(value.clone())));
            }
        }
    }
    let (expression, values) = stamped(&expression, values, now);
    ddb::update_item(
        ctx.client,
        &Update {
            table,
            key: subscriber_key(license_arn, account_id),
            expression: &expression,
            condition: None,
            values: Some(values),
            names: (!names.is_empty()).then_some(Json::Object(names)),
        },
    )?;
    Ok(())
}

/// The PII-free subscriber row: `productCode` and an idempotent `ADD` of this
/// Region to `registeredRegions` (a string set).
fn upsert_subscriber(
    ctx: &mut Ctx<'_>,
    license_arn: &str,
    account_id: &str,
    product_code: &str,
    now: &str,
) -> Result<(), Failure> {
    let region = ctx.config.text("AWS_REGION").to_string();
    let mut extra = Vec::new();
    if !region.is_empty() {
        let set = Json::object(vec![("SS", Json::Array(vec![Json::string(region)]))]);
        extra.push((":region".to_string(), set));
    }
    let (mut expression, values) = stamped_plus(
        "SET productCode = :pc",
        vec![(":pc".to_string(), Json::string(product_code))],
        extra,
        now,
    );
    if !ctx.config.text("AWS_REGION").is_empty() {
        expression.push_str(" ADD registeredRegions :region");
    }
    update(
        ctx,
        subscriber_key(license_arn, account_id),
        &expression,
        None,
        values,
    )?;
    Ok(())
}
