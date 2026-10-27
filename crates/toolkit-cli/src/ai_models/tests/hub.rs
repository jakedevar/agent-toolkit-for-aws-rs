use toolkit_aws::AwsError;
use toolkit_core::Json;

use super::{dumps, j, recorded, summary, Spy};
use crate::ai_models::client::Operation;
use crate::ai_models::hub::{
    customization_model_names, finetuning_techniques, get_deployable_models, list_hub_contents,
    list_request, validate_content_name, validate_endpoint_name, validate_hub_name, ModelsError,
};

#[test]
fn identifiers_follow_the_documented_patterns() {
    let long = "a".repeat(63);
    let too_long = "a".repeat(64);
    for good in [
        "a",
        "A1",
        "my-hub",
        "a--b",
        long.as_str(),
        "SageMakerPublicHub",
    ] {
        assert!(validate_hub_name(good).is_ok(), "{good}");
        assert!(validate_content_name(good).is_ok(), "{good}");
        assert!(validate_endpoint_name(good).is_ok(), "{good}");
    }
    for bad in [
        "",
        "-a",
        "a-",
        "a_b",
        "a b",
        "a/b",
        "a.b",
        too_long.as_str(),
        "a\u{1b}b",
        "a\nb",
        "é",
    ] {
        assert!(validate_hub_name(bad).is_err(), "{bad:?}");
        assert!(validate_content_name(bad).is_err(), "{bad:?}");
        assert!(validate_endpoint_name(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn a_hub_may_be_named_by_arn() {
    assert!(validate_hub_name("arn:aws:sagemaker:us-east-1:aws:hub/SageMakerPublicHub").is_ok());
    assert!(validate_hub_name("arn:aws:sagemaker:eu-west-2:123456789012:hub/team-hub").is_ok());
    for bad in [
        "arn:aws:sagemaker:us-east-1:aws:hub/",
        "arn:aws:s3:us-east-1:aws:hub/x",
        "arn:aws:sagemaker:us-east-1:12345:hub/x",
        "arn:aws:sagemaker:us-east-1:aws:model/x",
        "arn:aws:sagemaker:us-east-1:aws:hub/-x",
        "arn:aws:sagemaker:useast1:aws:hub/x",
    ] {
        assert!(validate_hub_name(bad).is_err(), "{bad}");
    }
}

#[test]
fn rejected_names_are_never_echoed() {
    let message = validate_hub_name("evil\u{1b}[31m\u{202e}FAKE-SECRET").unwrap_err();
    assert_eq!(
        message,
        "invalid Hub name: use 1-63 letters, digits and hyphens, starting and ending with a letter or digit"
    );
}

#[test]
fn listing_follows_next_token_with_the_documented_request() {
    let mut client = recorded(
        r#"{"calls": {"ListHubContents": [
            {"request": {"HubName": "h1", "HubContentType": "Model", "MaxResults": 100},
             "response": {"HubContentSummaries": [{"HubContentName": "a"}], "NextToken": "t1"}},
            {"request": {"HubName": "h1", "HubContentType": "Model", "MaxResults": 100, "NextToken": "t1"},
             "response": {"HubContentSummaries": [{"HubContentName": "b"}, {"HubContentName": "c"}], "NextToken": ""}}]}}"#,
    );
    let items = list_hub_contents(&mut client, "h1").expect("lists");
    let names: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("HubContentName").and_then(Json::as_str))
        .collect();
    assert_eq!(names, ["a", "b", "c"]);
    assert_eq!(
        dumps(&list_request("h1", None)),
        r#"{"HubName": "h1", "HubContentType": "Model", "MaxResults": 100}"#
    );
}

#[test]
fn a_missing_summaries_member_is_an_empty_page() {
    let mut client = recorded(r#"{"calls": {"ListHubContents": [{"response": {}}]}}"#);
    assert!(list_hub_contents(&mut client, "h").unwrap().is_empty());
}

#[test]
fn listing_failures_are_errors_not_partial_results() {
    let page = |token: &str| {
        j(&format!(
            r#"{{"HubContentSummaries": [], "NextToken": "{token}"}}"#
        ))
    };
    let mut stuck = Spy::new(vec![Ok(page("SECRET-TOKEN")), Ok(page("SECRET-TOKEN"))]);
    let error = list_hub_contents(&mut stuck, "h").unwrap_err();
    assert!(matches!(error, AwsError::Pagination { .. }), "{error}");
    assert!(!error.to_string().contains("SECRET"), "{error}");
    for bad in [
        r#"{"HubContentSummaries": "x"}"#,
        r#"{"HubContentSummaries": [1]}"#,
        r#"{"NextToken": 3}"#,
    ] {
        let mut client = Spy::new(vec![Ok(j(bad))]);
        assert!(list_hub_contents(&mut client, "h").is_err(), "{bad}");
    }
    let mut denied = recorded(
        r#"{"calls": {"ListHubContents": [{"response": {"HubContentSummaries": [], "NextToken": "t"}},
            {"error": {"code": "ThrottlingException", "status": 400}}]}}"#,
    );
    assert!(matches!(
        list_hub_contents(&mut denied, "h"),
        Err(AwsError::Service(_))
    ));
}

#[test]
fn deployable_models_are_normalized_records() {
    let record = summary(
        "m",
        &["@task:t", "@capability:customization"],
        Some("2024-01-01T00:00:00+00:00"),
    );
    let mut client = Spy::new(vec![Ok(Json::object(vec![(
        "HubContentSummaries",
        Json::Array(vec![record]),
    )]))]);
    let models = get_deployable_models(&mut client, "h").expect("lists");
    assert_eq!(
        dumps(&Json::Array(models)),
        concat!(
            r#"[{"name": "m", "tasks": ["t"], "bedrock_eligible": false, "customization_eligible": true, "#,
            r#""original_creation_time": "2024-01-01T00:00:00+00:00"}]"#
        )
    );
}

#[test]
fn a_record_with_bad_keywords_names_its_position() {
    let bad =
        j(r#"{"HubContentSummaries": [{"HubContentName": "a"}, {"HubContentSearchKeywords": 5}]}"#);
    let mut client = Spy::new(vec![Ok(bad)]);
    match get_deployable_models(&mut client, "h") {
        Err(ModelsError::Data(message)) => {
            assert_eq!(
                message,
                "model 1: HubContentSearchKeywords must be a list of strings when present (it is integer)"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn model_names_keep_customization_models_sorted_case_insensitively() {
    let summaries = Json::Array(vec![
        summary("zeta", &["@capability:customization"], None),
        summary("Alpha", &["@capability:customization"], None),
        summary("beta", &["@capability:other"], None),
        summary("alpha-2", &["@capability:customization"], None),
        summary("Beta-3", &["@capability:customization"], None),
        j(r#"{"HubContentSearchKeywords": ["@capability:customization"]}"#),
    ]);
    let mut client = Spy::new(vec![Ok(Json::object(vec![(
        "HubContentSummaries",
        summaries,
    )]))]);
    let names = customization_model_names(&mut client, "h").expect("lists");
    // A record without a name is kept as null and sorts first, like "".
    assert_eq!(
        dumps(&names),
        r#"[null, "Alpha", "alpha-2", "Beta-3", "zeta"]"#
    );
}

#[test]
fn recipes_are_the_sorted_unique_supported_techniques() {
    let keywords = [
        "@recipe:finetuning_sft_lora",
        "@recipe:finetuning_dpo_full",
        "@recipe:finetuning_sft_full",
        "@recipe:finetuning_rlvr_x",
        "@recipe:finetuning_rlaif",
        "@recipe:finetuning_unknown_x",
        "@recipe:other_sft",
        "@capability:customization",
    ];
    let detail = summary("m", &keywords, None);
    let mut client = Spy::new(vec![Ok(detail)]);
    let techniques = finetuning_techniques(&mut client, "h", "m").expect("describes");
    assert_eq!(dumps(&techniques), r#"["dpo", "rlaif", "rlvr", "sft"]"#);
    assert_eq!(
        client.seen,
        vec![(
            Operation::DescribeHubContent,
            j(r#"{"HubName": "h", "HubContentType": "Model", "HubContentName": "m"}"#)
        )]
    );
}

#[test]
fn an_unknown_model_is_a_service_error_not_an_empty_list() {
    let mut client = recorded(
        r#"{"calls": {"DescribeHubContent": [{"error": {"code": "ResourceNotFound", "message": "no such model"}}]}}"#,
    );
    assert!(matches!(
        finetuning_techniques(&mut client, "h", "nope"),
        Err(ModelsError::Aws(AwsError::Service(_)))
    ));
}
