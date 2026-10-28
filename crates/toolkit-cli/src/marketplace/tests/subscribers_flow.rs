//! The subscription, deprovision-cleanup and register Lambdas.

use super::*;

fn sqs_record(id: &str, body: &str) -> String {
    format!(
        r#"{{"messageId": "{id}", "body": {}}}"#,
        Json::string(body).to_compact_string()
    )
}

fn event_of(records: &[String]) -> String {
    format!(r#"{{"Records": [{}]}}"#, records.join(","))
}

fn lifecycle(detail_type: &str, detail: &str, time: Option<&str>) -> String {
    let time = time.map_or_else(String::new, |t| format!(r#", "time": "{t}""#));
    format!(r#"{{"detail-type": "{detail_type}", "detail": {detail}{time}}}"#)
}

fn detail(license: Option<&str>) -> String {
    let license = license.map_or_else(String::new, |arn| {
        format!(r#", "license": {{"arn": "{arn}"}}"#)
    });
    format!(
        r#"{{"acceptor": {{"accountId": "{ACCOUNT}"}}, "agreement": {{"id": "agmt-1", "endTime": "2027-01-01T00:00:00Z"}}, "product": {{"code": "prod123"}}{license}}}"#
    )
}

fn subscription(bodies: &[(&str, String)], calls: &[String]) -> Ran {
    let records: Vec<String> = bodies
        .iter()
        .map(|(id, body)| sqs_record(id, body))
        .collect();
    run("subscription", &event_of(&records), calls)
}

fn write_key() -> String {
    format!(r#"{{"licenseArn":"{LICENSE}","customerAWSAccountId":"{ACCOUNT}"}}"#)
}

#[test]
fn license_updated_upserts_the_active_subscriber_conditionally() {
    let body = lifecycle("License Updated", &detail(Some(LICENSE)), None);
    let ran = subscription(&[("m1", body)], &[update()]);
    assert_eq!(ran.ok().to_compact_string(), r#"{"batchItemFailures":[]}"#);
    let write = ran.requests("dynamodb:UpdateItem")[0];
    assert_eq!(
        write.to_compact_string(),
        format!(
            r#"{{"TableName":"subs","Key":{},"UpdateExpression":"SET productCode = :pc, agreementId = :aid, agreementStatus = :active, subscriptionStatus = :active, createdAt = if_not_exists(createdAt, :now), updatedAt = :now","ConditionExpression":"attribute_not_exists(subscriptionStatus) OR subscriptionStatus = :active","ExpressionAttributeValues":{{":pc":"prod123",":aid":"agmt-1",":active":"active",":now":"2026-01-31T12:30:00Z"}}}}"#,
            write_key()
        )
    );
    assert!(ran.logged("License updated (active): account=****3333 licenseArn=****c123"));
}

#[test]
fn a_late_license_updated_never_reactivates_a_revoked_license() {
    let body = lifecycle("License Updated", &detail(Some(LICENSE)), None);
    let calls = [
        fail("dynamodb", "UpdateItem", "ConditionalCheckFailedException"),
        update(),
    ];
    let ran = subscription(&[("m1", body)], &calls);
    assert_eq!(ran.ok().to_compact_string(), r#"{"batchItemFailures":[]}"#);
    let writes = ran.requests("dynamodb:UpdateItem");
    assert_eq!(writes.len(), 2);
    assert_eq!(
        writes[1].get("UpdateExpression").and_then(Json::as_str),
        Some("SET productCode = :pc, agreementId = :aid, agreementStatus = :active, createdAt = if_not_exists(createdAt, :now), updatedAt = :now")
    );
    assert!(writes[1].get("ConditionExpression").is_none());
    assert!(ran.logged("not reactivating a revoked license"));
}

#[test]
fn license_updated_without_an_identity_is_logged_and_not_written() {
    let no_arn = lifecycle("License Updated", &detail(None), None);
    let no_account = lifecycle(
        "License Updated",
        &format!(r#"{{"license": {{"arn": "{LICENSE}"}}}}"#),
        None,
    );
    let ran = subscription(&[("a", no_arn), ("b", no_account)], &[]);
    assert_eq!(ran.ok().to_compact_string(), r#"{"batchItemFailures":[]}"#);
    assert!(ran.sent.is_empty());
    assert_eq!(
        ran.log
            .iter()
            .filter(|l| l.contains("Missing acceptorAccountId or licenseArn"))
            .count(),
        2
    );
}

#[test]
fn deprovisioned_opens_the_flush_window_from_the_event_time() {
    let write = |time: Option<&str>| {
        let body = lifecycle("License Deprovisioned", &detail(Some(LICENSE)), time);
        let ran = subscription(&[("m1", body)], &[update()]);
        assert_eq!(ran.ok().to_compact_string(), r#"{"batchItemFailures":[]}"#);
        ran.requests("dynamodb:UpdateItem")[0].clone()
    };
    let request = write(Some("2026-01-31T10:00:00Z"));
    assert_eq!(
        request.get("UpdateExpression").and_then(Json::as_str),
        Some("SET subscriptionStatus = :s, deprovisioningPendingFlag = :f, deprovisioningExpiry = :exp, createdAt = if_not_exists(createdAt, :now), updatedAt = :now")
    );
    assert_eq!(
        request
            .get("ExpressionAttributeValues")
            .map(Json::to_compact_string)
            .as_deref(),
        Some(
            r#"{":s":"deprovisioning",":f":"1",":exp":"2026-01-31T11:00:00Z",":now":"2026-01-31T12:30:00Z"}"#
        )
    );
    assert!(request.get("ConditionExpression").is_none());
    // An offset is converted to UTC; a missing or bad time falls back to now.
    let offset = write(Some("2026-01-31T10:00:00+02:00"));
    assert!(offset
        .to_compact_string()
        .contains(r#"":exp":"2026-01-31T09:00:00Z""#.trim_start_matches('"')));
    assert!(write(None)
        .to_compact_string()
        .contains(r#"":exp":"2026-01-31T13:30:00Z""#.trim_start_matches('"')));
    assert!(write(Some("garbage"))
        .to_compact_string()
        .contains(r#"":exp":"2026-01-31T13:30:00Z""#.trim_start_matches('"')));
}

#[test]
fn a_missing_license_arn_is_resolved_from_the_agreement_index() {
    let body = lifecycle("Purchase Agreement Ended", &detail(None), None);
    let found = query(&format!(r#"{{"Items": [{{"licenseArn": "{LICENSE}"}}]}}"#));
    let ran = subscription(&[("m1", body.clone())], &[found, update()]);
    assert_eq!(ran.ok().to_compact_string(), r#"{"batchItemFailures":[]}"#);
    let lookup = ran.requests("dynamodb:Query")[0];
    assert_eq!(
        lookup.to_compact_string(),
        r#"{"TableName":"subs","IndexName":"agreementId-index","KeyConditionExpression":"agreementId = :agr","ExpressionAttributeValues":{":agr":"agmt-1"},"Limit":1}"#
    );
    let write = ran.requests("dynamodb:UpdateItem")[0];
    assert_eq!(
        write.get("UpdateExpression").and_then(Json::as_str),
        Some("SET agreementStatus = :inactive, createdAt = if_not_exists(createdAt, :now), updatedAt = :now")
    );
    assert_eq!(
        write.get("Key").map(Json::to_compact_string),
        Some(write_key())
    );
    // Unresolved: logged, no write.
    let ran = subscription(&[("m1", body.clone())], &[query(r#"{"Items": []}"#)]);
    assert!(ran.logged("Cannot resolve subscriber for Purchase Agreement Ended event"));
    assert!(ran.requests("dynamodb:UpdateItem").is_empty());
    // A permission error is not "not found": the message fails and is retried.
    let ran = subscription(
        &[("m1", body)],
        &[fail("dynamodb", "Query", "AccessDeniedException")],
    );
    assert_eq!(
        ran.ok().to_compact_string(),
        r#"{"batchItemFailures":[{"itemIdentifier":"m1"}]}"#
    );
}

#[test]
fn agreement_amended_refreshes_the_end_time_only() {
    let body = lifecycle("Purchase Agreement Amended", &detail(Some(LICENSE)), None);
    let ran = subscription(&[("m1", body)], &[update()]);
    let write = ran.requests("dynamodb:UpdateItem")[0];
    assert_eq!(
        write.get("UpdateExpression").and_then(Json::as_str),
        Some("SET endTime = :et, agreementStatus = :active, createdAt = if_not_exists(createdAt, :now), updatedAt = :now")
    );
    assert_eq!(
        write
            .get("ExpressionAttributeValues")
            .map(Json::to_compact_string)
            .as_deref(),
        Some(r#"{":et":"2027-01-01T00:00:00Z",":active":"active",":now":"2026-01-31T12:30:00Z"}"#)
    );
}

#[test]
fn created_and_unknown_events_write_nothing() {
    let created = lifecycle("Purchase Agreement Created", &detail(Some(LICENSE)), None);
    let other = lifecycle("Something Else", &detail(Some(LICENSE)), None);
    let ran = subscription(&[("a", created), ("b", other)], &[]);
    assert_eq!(ran.ok().to_compact_string(), r#"{"batchItemFailures":[]}"#);
    assert!(ran.sent.is_empty());
    assert!(ran.logged("Purchase Agreement Created received; not used by metering path (ignored)"));
}

#[test]
fn bad_bodies_are_skipped_and_failed_events_are_retried() {
    let good = lifecycle("License Updated", &detail(Some(LICENSE)), None);
    let records = [
        sqs_record("garbage", "{not json"),
        r#"{"messageId": "no-body"}"#.to_string(),
        sqs_record("no-type", r#"{"hello": 1}"#),
        sqs_record("fails", &good),
    ];
    let ran = run(
        "subscription",
        &event_of(&records),
        &[fail("dynamodb", "UpdateItem", "InternalServerError")],
    );
    assert_eq!(
        ran.ok().to_compact_string(),
        r#"{"batchItemFailures":[{"itemIdentifier":"fails"}]}"#
    );
    assert_eq!(
        ran.log
            .iter()
            .filter(|l| l.contains("Invalid SQS message body, skipping (not retried)"))
            .count(),
        2
    );
    assert!(ran.logged("Unknown message format, skipping"));
    assert!(ran.logged("Failed to process record: aws_service_error (InternalServerError)"));
    assert!(!ran.log.iter().any(|l| l.contains(SECRET)));
    // A detail section of the wrong type fails that message only.
    let broken = lifecycle("License Updated", r#"{"acceptor": 5}"#, None);
    let ran = subscription(&[("x", broken)], &[]);
    assert_eq!(
        ran.ok().to_compact_string(),
        r#"{"batchItemFailures":[{"itemIdentifier":"x"}]}"#
    );
}

// ---- deprovision-cleanup ---------------------------------------------------------

#[test]
fn expired_deprovisioning_licenses_become_inactive() {
    let items = format!(
        r#"{{"Items": [{{"licenseArn": "{LICENSE}", "customerAWSAccountId": "{ACCOUNT}"}}, {{"licenseArn": "{LICENSE}", "customerAWSAccountId": "999999999999"}}, {{"licenseArn": "", "customerAWSAccountId": "x"}}, {{"licenseArn": "arn:x"}}]}}"#
    );
    let calls = [
        query(&items),
        update(),
        fail("dynamodb", "UpdateItem", "ConditionalCheckFailedException"),
    ];
    let ran = run("deprovision-cleanup", "{}", &calls);
    assert_eq!(ran.ok().to_compact_string(), r#"{"finalized":1}"#);
    let lookup = ran.requests("dynamodb:Query")[0];
    assert_eq!(
        lookup.to_compact_string(),
        r#"{"TableName":"subs","IndexName":"deprovisioning-pending-index","KeyConditionExpression":"deprovisioningPendingFlag = :f AND deprovisioningExpiry <= :now","ExpressionAttributeValues":{":f":"1",":now":"2026-01-31T12:30:00Z"}}"#
    );
    let write = ran.requests("dynamodb:UpdateItem")[0];
    assert_eq!(
        write.get("UpdateExpression").and_then(Json::as_str),
        Some("SET subscriptionStatus = :inactive, updatedAt = :now REMOVE deprovisioningPendingFlag, deprovisioningExpiry")
    );
    assert_eq!(
        write.get("ConditionExpression").and_then(Json::as_str),
        Some("attribute_exists(deprovisioningPendingFlag)")
    );
    assert!(
        ran.logged("Finalized deprovisioning->inactive for licenseArn=****c123 (window elapsed)")
    );
    assert!(ran.logged("Deprovision cleanup: finalized 1 expired license(s) as inactive"));
}

#[test]
fn another_error_while_finalizing_fails_the_run() {
    let items = format!(
        r#"{{"Items": [{{"licenseArn": "{LICENSE}", "customerAWSAccountId": "{ACCOUNT}"}}]}}"#
    );
    let ran = run(
        "deprovision-cleanup",
        "{}",
        &[
            query(&items),
            fail(
                "dynamodb",
                "UpdateItem",
                "ProvisionedThroughputExceededException",
            ),
        ],
    );
    assert_eq!(
        ran.err(),
        "aws_service_error (ProvisionedThroughputExceededException)"
    );
}

// ---- register -------------------------------------------------------------------

fn api_event(body: &str) -> String {
    format!(r#"{{"body": {}}}"#, Json::string(body).to_compact_string())
}

fn reply(ran: &Ran) -> (i64, String) {
    let value = ran.ok();
    let status = match value.get("statusCode") {
        Some(Json::Int(status)) => *status,
        _ => -1,
    };
    let body = value
        .get("body")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_string();
    (status, body)
}

fn resolve(response: &str) -> String {
    step("meteringmarketplace", "ResolveCustomer", None, response)
}

fn resolved() -> String {
    resolve(&format!(
        r#"{{"CustomerAWSAccountId": "{ACCOUNT}", "LicenseArn": "{LICENSE}", "ProductCode": "prod123"}}"#
    ))
}

#[test]
fn register_validates_the_request_before_calling_anything() {
    let empty = run("register", &api_event(""), &[]);
    assert_eq!(
        reply(&empty),
        (400, r#"{"error": "Invalid request body"}"#.to_string())
    );
    let null_body = run("register", r#"{"body": null}"#, &[]);
    assert_eq!(reply(&null_body).0, 400);
    let huge = run("register", &api_event(&"a".repeat(16_385)), &[]);
    assert_eq!(reply(&huge).0, 400);
    let no_token = run(
        "register",
        &api_event("other=1&x-amzn-marketplace-token="),
        &[],
    );
    assert_eq!(
        reply(&no_token),
        (
            400,
            r#"{"error": "Missing x-amzn-marketplace-token"}"#.to_string()
        )
    );
    let blank = run(
        "register",
        &api_event("x-amzn-marketplace-token=%20%20"),
        &[],
    );
    assert_eq!(reply(&blank).0, 400);
    assert!(empty.sent.is_empty() && no_token.sent.is_empty() && blank.sent.is_empty());
    // The response carries the security headers of the script.
    let headers = empty
        .ok()
        .get("headers")
        .map(Json::to_compact_string)
        .unwrap_or_default();
    for header in [
        "no-store",
        "nosniff",
        "DENY",
        "max-age=31536000; includeSubDomains",
    ] {
        assert!(headers.contains(header), "{header}");
    }
}

#[test]
fn register_maps_resolve_customer_failures_to_the_scripts_replies() {
    let event = api_event("x-amzn-marketplace-token=tok");
    let expired = run(
        "register",
        &event,
        &[fail(
            "meteringmarketplace",
            "ResolveCustomer",
            "ExpiredTokenException",
        )],
    );
    assert_eq!(reply(&expired).0, 400);
    assert!(reply(&expired).1.contains("Registration token expired"));
    let invalid = run(
        "register",
        &event,
        &[fail(
            "meteringmarketplace",
            "ResolveCustomer",
            "InvalidTokenException",
        )],
    );
    assert_eq!(
        reply(&invalid),
        (
            400,
            r#"{"error": "Invalid registration token"}"#.to_string()
        )
    );
    let other = run(
        "register",
        &event,
        &[fail(
            "meteringmarketplace",
            "ResolveCustomer",
            "ThrottlingException",
        )],
    );
    assert_eq!(
        reply(&other),
        (
            500,
            r#"{"error": "Failed to resolve customer. Please try again."}"#.to_string()
        )
    );
    assert!(other.logged("ResolveCustomer failed: aws_service_error (ThrottlingException)"));
    assert!(!other.log.iter().any(|l| l.contains(SECRET)));
    assert!(!reply(&other).1.contains(SECRET));
    // The token goes to ResolveCustomer exactly as sent (percent-decoded).
    let decoded = run(
        "register",
        &api_event("x-amzn-marketplace-token=a%2Bb+c"),
        &[fail(
            "meteringmarketplace",
            "ResolveCustomer",
            "InvalidTokenException",
        )],
    );
    assert_eq!(
        decoded.requests("meteringmarketplace:ResolveCustomer")[0].to_compact_string(),
        r#"{"RegistrationToken":"a+b c"}"#
    );
}

#[test]
fn register_refuses_an_incomplete_or_foreign_resolution() {
    let event = api_event("x-amzn-marketplace-token=tok");
    let missing = run(
        "register",
        &event,
        &[resolve(r#"{"ProductCode": "prod123"}"#)],
    );
    assert_eq!(reply(&missing).0, 500);
    let wrong_type = run(
        "register",
        &event,
        &[resolve(
            r#"{"CustomerAWSAccountId": 5, "ProductCode": "prod123"}"#,
        )],
    );
    assert_eq!(reply(&wrong_type).0, 500);
    let foreign = run(
        "register",
        &event,
        &[resolve(&format!(
            r#"{{"CustomerAWSAccountId": "{ACCOUNT}", "LicenseArn": "{LICENSE}", "ProductCode": "other"}}"#
        ))],
    );
    assert_eq!(
        reply(&foreign),
        (400, r#"{"error": "Product code mismatch"}"#.to_string())
    );
    assert!(foreign.logged("Product code mismatch: received=other expected=prod123"));
    assert!(foreign.requests("dynamodb:UpdateItem").is_empty());
    let no_license = run(
        "register",
        &event,
        &[resolve(&format!(
            r#"{{"CustomerAWSAccountId": "{ACCOUNT}", "ProductCode": "prod123"}}"#
        ))],
    );
    assert_eq!(reply(&no_license).0, 200);
    assert!(reply(&no_license).1.contains("Registration received"));
    assert!(no_license.requests("dynamodb:UpdateItem").is_empty());
}

#[test]
fn register_writes_the_profile_in_region_and_a_pii_free_subscriber_row() {
    let cfg = [
        ("SUBSCRIBERS_TABLE", "subs"),
        ("PRODUCT_CODE", "prod123"),
        ("CUSTOMER_PROFILE_TABLE", "profiles"),
        ("AWS_REGION", "eu-west-1"),
        ("ALLOWED_REGISTRATION_FIELDS", "company-name, email,team"),
        ("PROMOTED_PROFILE_FIELDS", "email,company-name"),
        ("MAX_FIELD_VALUE_LENGTH", "10"),
        ("MAX_REGISTRATION_FIELDS", "2"),
    ];
    let body = "x-amzn-marketplace-token=tok&company-name=Acme+Corp+International&email=a%40b.co&team=eng&other=drop";
    let ran = run_with(
        "register",
        &cfg,
        &api_event(body),
        &[resolved(), update(), update()],
        NOW,
    );
    assert_eq!(
        reply(&ran),
        (
            200,
            r#"{"message": "Registration successful. Your account is being activated."}"#
                .to_string()
        )
    );
    let writes = ran.requests("dynamodb:UpdateItem");
    // The profile row: company-name truncated to 10 characters, only two fields kept.
    assert_eq!(
        writes[0].to_compact_string(),
        format!(
            r##"{{"TableName":"profiles","Key":{},"UpdateExpression":"SET productCode = :pc, registrationData = :rd, #n0 = :v0, #n1 = :v1, createdAt = if_not_exists(createdAt, :now), updatedAt = :now","ExpressionAttributeValues":{{":pc":"prod123",":rd":{{"company-name":"Acme Corp ","email":"a@b.co"}},":v0":"a@b.co",":v1":"Acme Corp ",":now":"2026-01-31T12:30:00Z"}},"ExpressionAttributeNames":{{"#n0":"email","#n1":"company-name"}}}}"##,
            write_key()
        )
    );
    // The subscriber row holds no registration data, only the product and the region set.
    assert_eq!(
        writes[1].to_compact_string(),
        format!(
            r#"{{"TableName":"subs","Key":{},"UpdateExpression":"SET productCode = :pc, createdAt = if_not_exists(createdAt, :now), updatedAt = :now ADD registeredRegions :region","ExpressionAttributeValues":{{":pc":"prod123",":now":"2026-01-31T12:30:00Z",":region":{{"SS":["eu-west-1"]}}}}}}"#,
            write_key()
        )
    );
    assert!(ran.logged("Registration field 'company-name' exceeds max length; truncating"));
    let subscriber = writes[1].to_compact_string();
    assert!(!subscriber.contains("a@b.co") && !subscriber.contains("Acme"));
}

#[test]
fn register_without_form_fields_still_writes_the_profile_and_decodes_base64() {
    let cfg = [
        ("SUBSCRIBERS_TABLE", "subs"),
        ("PRODUCT_CODE", "prod123"),
        ("CUSTOMER_PROFILE_TABLE", "profiles"),
    ];
    // "x-amzn-marketplace-token=tok" in base64.
    let event = r#"{"body": "eC1hbXpuLW1hcmtldHBsYWNlLXRva2VuPXRvaw==", "isBase64Encoded": true}"#;
    let ran = run_with(
        "register",
        &cfg,
        event,
        &[resolved(), update(), update()],
        NOW,
    );
    assert_eq!(reply(&ran).0, 200);
    let writes = ran.requests("dynamodb:UpdateItem");
    assert_eq!(
        writes[0].get("UpdateExpression").and_then(Json::as_str),
        Some("SET productCode = :pc, createdAt = if_not_exists(createdAt, :now), updatedAt = :now")
    );
    // No Region: no ADD clause.
    assert_eq!(
        writes[1].get("UpdateExpression").and_then(Json::as_str),
        Some("SET productCode = :pc, createdAt = if_not_exists(createdAt, :now), updatedAt = :now")
    );
    assert!(writes[0].get("ExpressionAttributeNames").is_none());
}

#[test]
fn register_fails_loud_without_a_profile_store_or_with_bad_base64() {
    let cfg = [("SUBSCRIBERS_TABLE", "subs"), ("PRODUCT_CODE", "prod123")];
    let ran = run_with(
        "register",
        &cfg,
        &api_event("x-amzn-marketplace-token=tok"),
        &[resolved()],
        NOW,
    );
    assert_eq!(
        reply(&ran),
        (
            500,
            r#"{"error": "Registration store not configured. Please try again."}"#.to_string()
        )
    );
    assert!(ran.requests("dynamodb:UpdateItem").is_empty());
    let bad = run(
        "register",
        r#"{"body": "abcde", "isBase64Encoded": true}"#,
        &[],
    );
    assert!(bad.err().starts_with("invalid base64 body"));
    let cfg_missing = run_with(
        "register",
        &[("SUBSCRIBERS_TABLE", "s")],
        &api_event("x"),
        &[],
        NOW,
    );
    assert_eq!(cfg_missing.err(), "PRODUCT_CODE is not configured");
}
