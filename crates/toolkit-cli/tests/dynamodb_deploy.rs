//! End-to-end tests of `toolkit dynamodb deploy-model` and `generate-teardown`
//! (#4391): real process spawns, real exit codes, separated stdout/stderr.
//! Fixtures are deterministic offline directories; nothing contacts AWS and no
//! generated script is run against AWS (the scripts are only syntax-checked or
//! run to their `--confirm` refusal, which precedes every `aws` call).

use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use toolkit_core::{parse_json, Json};

const BIN: &str = env!("CARGO_BIN_EXE_toolkit");
const RUN_ID: &str = "0123456789abcdef0123456789abcdef";
const CREATED_AT: &str = "2026-01-31T12:00:00Z";

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temp_root(label: &str) -> TempDir {
    let unique = format!(
        "toolkit-ddb-deploy-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let path = std::env::temp_dir().join(unique);
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create temp root");
    TempDir(path)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8 output")
}

fn status(output: &Output) -> i32 {
    output.status.code().expect("exit code")
}

fn toolkit(args: &[&str], cwd: &Path) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .env_clear()
        .output()
        .expect("spawn toolkit")
}

fn json(text: &str) -> Json {
    parse_json(text).expect("valid json")
}

fn items(value: &Json) -> &[Json] {
    match value {
        Json::Array(items) => items,
        other => panic!("expected an array, got {}", other.type_name()),
    }
}

fn at<'a>(value: &'a Json, path: &[&str]) -> &'a Json {
    let mut current = value;
    for key in path {
        current = current
            .get(key)
            .unwrap_or_else(|| panic!("missing {key} in {path:?}"));
    }
    current
}

fn str_at<'a>(value: &'a Json, path: &[&str]) -> &'a str {
    at(value, path).as_str().expect("a string")
}

const MODEL: &str = r#"{
  "tables": [
    {"table_name": "Orders",
     "key_schema": {"partition_key": "pk", "sort_key": "sk"},
     "entities": [{"attributes": [
        {"name": "pk", "type": "S"}, {"name": "sk", "type": "n"}, {"name": "gsi1pk", "type": "S"}]}],
     "attribute_definitions": [{"attribute_name": "sk", "attribute_type": "S"}],
     "gsis": [{"index_name": "ByCustomer", "partition_key": "gsi1pk", "sort_key": "sk",
               "projection": {"type": "include", "non_key_attributes": ["total"]}}],
     "streams": {"enabled": true}},
    {"table_name": "Docs", "key_schema": {"partition_key": "id"},
     "entities": [{"attributes": [{"name": "id", "type": "N"}]}],
     "vector_indexes": [{"index_name": "Emb", "vector_attribute": "vec", "dimensions": "1024",
        "distance_function": "cosine", "projection": "KEYS_ONLY",
        "search_schema": {"partition_key": "cat", "inline_filters": ["lang"]}}]}
  ],
  "access_patterns": [
    {"pattern_id": "P1", "operation": "Query", "table": "Orders", "index": "ByCustomer", "peak_rps": 10},
    {"pattern_id": "P2", "operation": "SearchVectors", "table": "Docs", "index": "Emb", "peak_rps": 5}
  ]
}"#;

const CONFIG: &str = r#"{"aws_profile": "bench", "region": "us-east-1",
  "resource_prefix": "ddb-skill-bench-test", "tags": {"team": "data", "purpose": "mine"}}"#;

const RECORDED: &str = r#"{"calls": [
  {"service": "sts", "operation": "GetCallerIdentity", "request": {},
   "response": {"Account": "111122223333", "Arn": "arn:aws:iam::111122223333:user/dev"}},
  {"service": "iam", "operation": "ListAccountAliases", "request": {},
   "response": {"AccountAliases": ["acme-sandbox"]}}
]}"#;

struct Run {
    dir: TempDir,
    output: Output,
}

fn deploy_with(model: &str, config: &str, recorded: &str, extra: &[&str]) -> Run {
    let dir = temp_root("deploy");
    fs::write(dir.join("model.json"), model).expect("write model");
    fs::write(dir.join("config.json"), config).expect("write config");
    fs::write(dir.join("rec.json"), recorded).expect("write recording");
    let mut args = vec![
        "dynamodb",
        "deploy-model",
        "--model",
        "model.json",
        "--config",
        "config.json",
        "--manifest-out",
        "created_resources.json",
        "--yes-deploy",
        "--recorded",
        "rec.json",
        "--plan-out",
        "plan.json",
        "--created-at",
        CREATED_AT,
        "--run-id",
        RUN_ID,
    ];
    args.extend_from_slice(extra);
    let output = toolkit(&args, &dir);
    Run { dir, output }
}

fn deploy() -> Run {
    deploy_with(MODEL, CONFIG, RECORDED, &[])
}

fn plan_of(run: &Run) -> Json {
    json(&fs::read_to_string(run.dir.join("plan.json")).expect("plan written"))
}

/// A refused run: exit 2, the stderr message, and nothing written.
fn refused(run: &Run) -> String {
    assert_eq!(status(&run.output), 2, "{}", text(&run.output.stderr));
    assert!(!run.dir.join("plan.json").exists(), "no plan on refusal");
    assert!(!run.dir.join("created_resources.json").exists());
    text(&run.output.stderr)
}

#[test]
fn the_plan_lists_every_resource_a_live_deploy_would_create_and_creates_none() {
    let run = deploy();
    assert_eq!(status(&run.output), 0, "{}", text(&run.output.stderr));
    let stdout = text(&run.output.stdout);
    assert!(stdout.contains("# PLAN ONLY — this command creates and changes no AWS resource."));
    assert!(stdout.contains("  Account: 111122223333\n  Alias:   \"acme-sandbox\"\n"));
    assert!(stdout.contains("PLAN ONLY — resources a live deploy would create (nothing was created):\n  table: ddb-skill-bench-test-Orders (from Orders)\n  table: ddb-skill-bench-test-Docs (from Docs)\n"));
    assert!(stdout.contains("  lambda: ddb-skill-bench-test-bench  (not built: this build only plans)\n  role:   ddb-skill-bench-test-bench-role\n"));
    assert!(!run.dir.join("created_resources.json").exists());
    assert!(stdout.contains("No manifest was written to created_resources.json"));

    let plan = plan_of(&run);
    assert_eq!(at(&plan, &["executed"]), &Json::Bool(false));
    assert_eq!(str_at(&plan, &["prefix"]), "ddb-skill-bench-test-");
    assert_eq!(str_at(&plan, &["run_id"]), RUN_ID);
    assert_eq!(str_at(&plan, &["created_at"]), CREATED_AT);
    assert_eq!(str_at(&plan, &["account"]), "111122223333");
    // `{**cfg_tags, **built_in_tags}`: the built-in value replaces a config
    // tag in place, new built-ins follow.
    let tags = at(&plan, &["tags"]);
    assert_eq!(
        tags.to_compact_string(),
        format!(
            "{{\"team\":\"data\",\"purpose\":\"ddb-skill-bench\",\"run_id\":\"{RUN_ID}\",\
             \"created_at\":\"{CREATED_AT}\",\"prefix\":\"ddb-skill-bench-test-\"}}"
        )
    );
}

#[test]
fn the_orders_create_table_request_matches_the_script() {
    let run = deploy();
    let plan = plan_of(&run);
    let orders = &items(at(&plan, &["tables"]))[0];
    assert_eq!(str_at(orders, &["original_name"]), "Orders");
    let request = at(orders, &["create_table"]);
    assert_eq!(
        str_at(request, &["TableName"]),
        "ddb-skill-bench-test-Orders"
    );
    assert_eq!(str_at(request, &["BillingMode"]), "PAY_PER_REQUEST");
    assert_eq!(
        at(request, &["DeletionProtectionEnabled"]),
        &Json::Bool(false)
    );
    // attribute_definitions (S) beats entities (N) for `sk`; names are sorted.
    assert_eq!(
        at(request, &["AttributeDefinitions"]).to_compact_string(),
        "[{\"AttributeName\":\"gsi1pk\",\"AttributeType\":\"S\"},\
         {\"AttributeName\":\"pk\",\"AttributeType\":\"S\"},\
         {\"AttributeName\":\"sk\",\"AttributeType\":\"S\"}]"
    );
    assert_eq!(
        at(request, &["KeySchema"]).to_compact_string(),
        "[{\"AttributeName\":\"pk\",\"KeyType\":\"HASH\"},{\"AttributeName\":\"sk\",\"KeyType\":\"RANGE\"}]"
    );
    assert_eq!(
        at(request, &["GlobalSecondaryIndexes"]).to_compact_string(),
        "[{\"IndexName\":\"ByCustomer\",\"KeySchema\":[{\"AttributeName\":\"gsi1pk\",\"KeyType\":\"HASH\"},\
         {\"AttributeName\":\"sk\",\"KeyType\":\"RANGE\"}],\
         \"Projection\":{\"ProjectionType\":\"INCLUDE\",\"NonKeyAttributes\":[\"total\"]}}]"
    );
    assert_eq!(
        at(request, &["StreamSpecification"]).to_compact_string(),
        "{\"StreamEnabled\":true,\"StreamViewType\":\"NEW_AND_OLD_IMAGES\"}"
    );
    assert_eq!(items(at(request, &["Tags"])).len(), 5);
}

#[test]
fn the_vector_table_request_declares_its_index_and_search_attributes() {
    let run = deploy();
    let plan = plan_of(&run);
    let docs = &items(at(&plan, &["tables"]))[1];
    let request = at(docs, &["create_table"]);
    // `id` is numeric from entities; the search schema attributes default to S.
    assert_eq!(
        at(request, &["AttributeDefinitions"]).to_compact_string(),
        "[{\"AttributeName\":\"cat\",\"AttributeType\":\"S\"},{\"AttributeName\":\"id\",\"AttributeType\":\"N\"},\
         {\"AttributeName\":\"lang\",\"AttributeType\":\"S\"}]"
    );
    assert_eq!(
        at(request, &["VectorIndexes"]).to_compact_string(),
        "[{\"IndexName\":\"Emb\",\"VectorAttribute\":{\"AttributeName\":\"vec\"},\
         \"Projection\":{\"ProjectionType\":\"KEYS_ONLY\"},\"Dimensions\":1024,\
         \"DistanceFunction\":\"COSINE\",\"SearchSchema\":[\
         {\"AttributeName\":\"cat\",\"SearchSchemaElementType\":\"HASH\"},\
         {\"AttributeName\":\"lang\",\"SearchSchemaElementType\":\"INLINE_FILTER\"}]}]"
    );
    assert_eq!(
        at(&plan, &["lambda", "code_package"]).to_compact_string(),
        "{\"module\":\"benchmark.py\",\"source\":\"scripts/benchmark_lambda.py\",\
         \"bundle_vector_sdk\":true,\"vector_sdk_min_botocore\":\"1.43.64\"}"
    );
}

#[test]
fn the_iam_documents_are_exactly_the_scripts_and_scoped_to_the_run() {
    let run = deploy();
    let plan = plan_of(&run);
    assert_eq!(
        str_at(&plan, &["iam", "create_role", "AssumeRolePolicyDocument"]),
        "{\"Version\": \"2012-10-17\", \"Statement\": [{\"Effect\": \"Allow\", \"Principal\": \
         {\"Service\": \"lambda.amazonaws.com\"}, \"Action\": \"sts:AssumeRole\", \"Condition\": \
         {\"StringEquals\": {\"aws:SourceAccount\": \"111122223333\"}}}]}"
    );
    let table = "arn:aws:dynamodb:us-east-1:111122223333:table/ddb-skill-bench-test-*";
    let expected = format!(
        "{{\"Version\": \"2012-10-17\", \"Statement\": [\
         {{\"Effect\": \"Allow\", \"Action\": [\"dynamodb:DescribeTable\", \"dynamodb:GetItem\", \
         \"dynamodb:Query\", \"dynamodb:Scan\", \"dynamodb:BatchGetItem\", \"dynamodb:PutItem\", \
         \"dynamodb:UpdateItem\", \"dynamodb:DeleteItem\", \"dynamodb:BatchWriteItem\", \
         \"dynamodb:TransactWriteItems\", \"dynamodb:TransactGetItems\"], \
         \"Resource\": [\"{table}\", \"{table}/index/*\"]}}, \
         {{\"Effect\": \"Allow\", \"Action\": [\"dynamodb:SearchVectors\"], \
         \"Resource\": [\"{table}/index/*\"]}}, \
         {{\"Effect\": \"Allow\", \"Action\": [\"logs:CreateLogGroup\", \"logs:CreateLogStream\", \
         \"logs:PutLogEvents\"], \"Resource\": \"arn:aws:logs:us-east-1:111122223333:log-group:\
         /aws/lambda/ddb-skill-bench-test-*:*\"}}]}}"
    );
    assert_eq!(
        str_at(&plan, &["iam", "put_role_policy", "PolicyDocument"]),
        expected
    );
    assert_eq!(
        str_at(&plan, &["iam", "put_role_policy", "PolicyName"]),
        "ddb-skill-bench-test-bench-policy"
    );
    let function = at(&plan, &["lambda", "create_function"]);
    assert_eq!(
        str_at(function, &["Role"]),
        "arn:aws:iam::111122223333:role/ddb-skill-bench-test-bench-role"
    );
    assert_eq!(str_at(function, &["Runtime"]), "python3.12");
    assert_eq!(str_at(function, &["Handler"]), "benchmark.handler");
    assert_eq!(at(function, &["MemorySize"]), &Json::Int(1769));
    assert_eq!(at(function, &["Timeout"]), &Json::Int(900));
    assert_eq!(
        at(function, &["Architectures"]).to_compact_string(),
        "[\"arm64\"]"
    );
}

#[test]
fn a_missing_prefix_is_generated_from_the_run_clock() {
    let config = r#"{"aws_profile": "bench", "region": "us-east-1"}"#;
    let run = deploy_with(MODEL, config, RECORDED, &[]);
    assert_eq!(status(&run.output), 0, "{}", text(&run.output.stderr));
    let plan = plan_of(&run);
    let prefix = str_at(&plan, &["prefix"]);
    assert!(
        prefix.starts_with("ddb-skill-bench-20260131-") && prefix.ends_with('-'),
        "{prefix}"
    );
    let token = &prefix["ddb-skill-bench-20260131-".len()..prefix.len() - 1];
    assert!(
        token.len() == 8 && token.bytes().all(|b| b.is_ascii_hexdigit()),
        "{token}"
    );
    assert!(text(&run.output.stdout)
        .contains("No resource_prefix supplied — generated \"ddb-skill-bench-20260131-"));
}

#[test]
fn a_trailing_dash_run_in_the_prefix_collapses_to_one() {
    let config = r#"{"aws_profile": "bench", "region": "us-east-1", "resource_prefix": "ddb-skill-bench-x---"}"#;
    let run = deploy_with(MODEL, config, RECORDED, &[]);
    assert_eq!(status(&run.output), 0, "{}", text(&run.output.stderr));
    assert_eq!(str_at(&plan_of(&run), &["prefix"]), "ddb-skill-bench-x-");
}

#[test]
fn dry_run_prints_the_intended_resources_and_the_global_provisioned_ceiling() {
    let config = r#"{"aws_profile": "bench", "region": "us-east-1", "resource_prefix": "ddb-skill-bench-t",
        "dry_run": true, "provisioned_capacity": {"read": 7}}"#;
    let model = MODEL.replace(
        ", \"vector_indexes\": [{\"index_name\": \"Emb\", \"vector_attribute\": \"vec\", \"dimensions\": \"1024\",\n        \"distance_function\": \"cosine\", \"projection\": \"KEYS_ONLY\",\n        \"search_schema\": {\"partition_key\": \"cat\", \"inline_filters\": [\"lang\"]}}]",
        "",
    );
    let model = model
        .replace("\"vector_indexes\": [{\"index_name\": \"Emb\", \"vector_attribute\": \"vec\", \"dimensions\": \"1024\",\n        \"distance_function\": \"cosine\", \"projection\": \"KEYS_ONLY\",\n        \"search_schema\": {\"partition_key\": \"cat\", \"inline_filters\": [\"lang\"]}}]", "\"x\": 1")
        .replace(
            "{\"pattern_id\": \"P2\", \"operation\": \"SearchVectors\", \"table\": \"Docs\", \"index\": \"Emb\", \"peak_rps\": 5}",
            "{\"pattern_id\": \"P2\", \"operation\": \"Scan\", \"table\": \"Docs\", \"peak_rps\": 5}",
        );
    let run = deploy_with(&model, config, RECORDED, &[]);
    assert_eq!(status(&run.output), 0, "{}", text(&run.output.stderr));
    let stdout = text(&run.output.stdout);
    assert!(stdout.contains("Provisioned-capacity mode: {'read': 7} (hard ceiling; bench-only — production designs default to on-demand).\n"));
    assert!(stdout.contains(
        "DRY RUN — intended resources:\n  table: ddb-skill-bench-t-Orders (from Orders)\n"
    ));
    let plan = plan_of(&run);
    let request = at(&items(at(&plan, &["tables"]))[0], &["create_table"]);
    assert_eq!(str_at(request, &["BillingMode"]), "PROVISIONED");
    // write defaults to 5; the GSI carries the same throughput.
    let expected = "{\"ReadCapacityUnits\":7,\"WriteCapacityUnits\":5}";
    assert_eq!(
        at(request, &["ProvisionedThroughput"]).to_compact_string(),
        expected
    );
    let gsi = &items(at(request, &["GlobalSecondaryIndexes"]))[0];
    assert_eq!(
        at(gsi, &["ProvisionedThroughput"]).to_compact_string(),
        expected
    );
}

#[test]
fn consent_is_required_before_anything_is_read() {
    let dir = temp_root("consent");
    let output = toolkit(
        &[
            "dynamodb",
            "deploy-model",
            "--model",
            "missing.json",
            "--config",
            "missing.json",
            "--manifest-out",
            "m.json",
        ],
        &dir,
    );
    assert_eq!(status(&output), 2);
    assert_eq!(
        text(&output.stderr),
        "error: refusing to deploy without --yes-deploy\n"
    );
    assert!(text(&output.stdout).is_empty());
}

#[test]
fn missing_options_are_usage_errors() {
    let dir = temp_root("usage");
    for args in [
        vec![
            "dynamodb",
            "deploy-model",
            "--config",
            "c",
            "--manifest-out",
            "m",
            "--yes-deploy",
        ],
        vec![
            "dynamodb",
            "deploy-model",
            "--model",
            "m",
            "--manifest-out",
            "m",
            "--yes-deploy",
        ],
        vec![
            "dynamodb",
            "deploy-model",
            "--model",
            "m",
            "--config",
            "c",
            "--yes-deploy",
        ],
        vec![
            "dynamodb",
            "deploy-model",
            "--model",
            "m",
            "--config",
            "c",
            "--manifest-out",
            "m",
            "--yes-deploy",
            "--recorded",
            "r.json",
        ],
        vec!["dynamodb", "generate-teardown", "--out", "t.sh"],
        vec!["dynamodb", "generate-teardown", "--manifest", "m.json"],
    ] {
        let output = toolkit(&args, &dir);
        assert_eq!(status(&output), 2, "{args:?}");
        assert!(
            text(&output.stderr).contains("the following arguments are required: --"),
            "{args:?}"
        );
    }
}

#[test]
fn without_a_recording_the_build_has_no_aws_client_and_writes_nothing() {
    let dir = temp_root("nolive");
    fs::write(dir.join("model.json"), MODEL).expect("model");
    fs::write(dir.join("config.json"), CONFIG).expect("config");
    let output = toolkit(
        &[
            "dynamodb",
            "deploy-model",
            "--model",
            "model.json",
            "--config",
            "config.json",
            "--manifest-out",
            "created_resources.json",
            "--yes-deploy",
        ],
        &dir,
    );
    assert_eq!(status(&output), 2);
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with("error: cannot reach AWS endpoint: "),
        "{stderr}"
    );
    assert!(
        stderr.contains("this build has no live AWS client"),
        "{stderr}"
    );
    assert!(!dir.join("created_resources.json").exists());
    assert!(!dir.join("plan.json").exists());
    // Design errors are still found first, like the script's validation order.
    fs::write(dir.join("model.json"), r#"{"tables": []}"#).expect("model");
    let output = toolkit(
        &[
            "dynamodb",
            "deploy-model",
            "--model",
            "model.json",
            "--config",
            "config.json",
            "--manifest-out",
            "created_resources.json",
            "--yes-deploy",
        ],
        &dir,
    );
    assert_eq!(text(&output.stderr), "error: design JSON has no tables\n");
}

#[test]
fn design_problems_are_refused_before_any_identity_call() {
    let cases: [(&str, String, &str); 12] = [
        ("no tables", r#"{"tables": [], "access_patterns": [{"peak_rps": 1}]}"#.to_string(), "design JSON has no tables"),
        (
            "no patterns",
            r#"{"tables": [{"table_name": "T1", "key_schema": {"partition_key": "p"}}]}"#.to_string(),
            "design JSON has no access_patterns — refusing (Mechanics #2:",
        ),
        (
            "zero rps",
            r#"{"tables": [{"table_name": "T1", "key_schema": {"partition_key": "p"}}],
                "access_patterns": [{"pattern_id": "A", "table": "T1", "peak_rps": 0}]}"#.to_string(),
            "access pattern \"A\" has missing or zero peak_rps — refusing per Mechanics #2.",
        ),
        (
            "no partition key",
            r#"{"tables": [{"table_name": "T1", "key_schema": {"sort_key": "s"}}],
                "access_patterns": [{"table": "T1", "peak_rps": 1}]}"#.to_string(),
            "table \"T1\" has no key_schema.partition_key. Live deploy requires",
        ),
        (
            "unknown table",
            r#"{"tables": [{"table_name": "T1", "key_schema": {"partition_key": "p"}}],
                "access_patterns": [{"pattern_id": "A", "table": "Nope", "peak_rps": 1}]}"#.to_string(),
            "access pattern \"A\" references table \"Nope\", which is not defined in tables[]. Defined tables: [\"T1\"].",
        ),
        (
            "unknown gsi",
            r#"{"tables": [{"table_name": "T1", "key_schema": {"partition_key": "p"}}],
                "access_patterns": [{"pattern_id": "A", "table": "T1", "index": "G", "operation": "Query", "peak_rps": 1}]}"#.to_string(),
            "access pattern \"A\" uses index \"G\" on table \"T1\", but that table defines no such GSI. Defined GSIs on \"T1\": [].",
        ),
        (
            "query on a vector index",
            r#"{"tables": [{"table_name": "T1", "key_schema": {"partition_key": "p"},
                  "vector_indexes": [{"index_name": "V"}]}],
                "access_patterns": [{"pattern_id": "A", "table": "T1", "index": "V", "operation": "Query", "peak_rps": 1}]}"#.to_string(),
            "but \"V\" is a VECTOR index. Query and Scan are rejected",
        ),
        (
            "search without index",
            r#"{"tables": [{"table_name": "T1", "key_schema": {"partition_key": "p"}}],
                "access_patterns": [{"pattern_id": "A", "table": "T1", "operation": "SearchVectors", "peak_rps": 1}]}"#.to_string(),
            "access pattern \"A\" is a SearchVectors but has no \"index\"",
        ),
        (
            "include without attributes",
            r#"{"tables": [{"table_name": "T1", "key_schema": {"partition_key": "p"},
                  "gsis": [{"index_name": "G", "partition_key": "g", "projection": {"type": "INCLUDE"}}]}],
                "access_patterns": [{"pattern_id": "A", "table": "T1", "peak_rps": 1}]}"#.to_string(),
            "GSI \"G\" on table \"T1\" has projection type INCLUDE but no projected attributes.",
        ),
        (
            "gsi without partition key",
            r#"{"tables": [{"table_name": "T1", "key_schema": {"partition_key": "p"},
                  "gsis": [{"index_name": "G"}]}],
                "access_patterns": [{"pattern_id": "A", "table": "T1", "peak_rps": 1}]}"#.to_string(),
            "GSI \"G\" on table \"T1\" has no partition_key",
        ),
        (
            "vector with provisioned capacity",
            r#"{"tables": [{"table_name": "T1", "key_schema": {"partition_key": "p"},
                  "provisioned_capacity": {"read": 1, "write": 1},
                  "vector_indexes": [{"index_name": "V", "vector_attribute": "v", "dimensions": 8, "distance_function": "COSINE"}]}],
                "access_patterns": [{"pattern_id": "A", "table": "T1", "operation": "SearchVectors", "index": "V", "peak_rps": 1}]}"#.to_string(),
            "declares a vector index AND provisioned capacity.",
        ),
        (
            "bad dimensions",
            r#"{"tables": [{"table_name": "T1", "key_schema": {"partition_key": "p"},
                  "vector_indexes": [{"index_name": "V", "vector_attribute": "v", "dimensions": 9000, "distance_function": "COSINE"}]}],
                "access_patterns": [{"pattern_id": "A", "table": "T1", "operation": "SearchVectors", "index": "V", "peak_rps": 1}]}"#.to_string(),
            "vector index \"V\": dimensions must be 1-4096, got \"9000\"",
        ),
    ];
    for (label, model, expected) in cases {
        let run = deploy_with(&model, CONFIG, RECORDED, &[]);
        let stderr = refused(&run);
        assert!(
            stderr.starts_with("error: ") && stderr.contains(expected),
            "{label}: {stderr}"
        );
        // Errors found while building a table's request come after the
        // identity banner, like the script's order; every other design error
        // precedes any identity call.
        let builds = [
            "include without attributes",
            "gsi without partition key",
            "vector with provisioned capacity",
            "bad dimensions",
        ];
        if !builds.contains(&label) {
            assert!(
                text(&run.output.stdout).is_empty(),
                "{label}: no banner before the design is valid"
            );
        }
    }
}

#[test]
fn config_problems_are_refused_and_names_cannot_widen_the_policy() {
    let cases: [(&str, &str, &str); 8] = [
        ("no profile", r#"{"region": "us-east-1"}"#, "benchmark_config.json missing required field: aws_profile"),
        ("no region", r#"{"aws_profile": "b"}"#, "benchmark_config.json missing required field: region"),
        (
            "foreign prefix",
            r#"{"aws_profile": "b", "region": "us-east-1", "resource_prefix": "prod-"}"#,
            "resource_prefix must start with 'ddb-skill-bench-' so teardown can scope deletions safely.",
        ),
        (
            "wildcard prefix",
            r#"{"aws_profile": "b", "region": "us-east-1", "resource_prefix": "ddb-skill-bench-*"}"#,
            "resource_prefix must be at most 48 characters of letters, digits, '-' and '_'",
        ),
        (
            "long prefix",
            r#"{"aws_profile": "b", "region": "us-east-1", "resource_prefix": "ddb-skill-bench-0123456789012345678901234567890123"}"#,
            "must be at most 48 characters",
        ),
        (
            "bad region",
            r#"{"aws_profile": "b", "region": "us-east-1\"x", "resource_prefix": "ddb-skill-bench-a"}"#,
            "is not an AWS region name",
        ),
        (
            "wrong type prefix",
            r#"{"aws_profile": "b", "region": "us-east-1", "resource_prefix": 5}"#,
            "\"resource_prefix\" must be a string (it is integer)",
        ),
        (
            "bad lambda memory",
            r#"{"aws_profile": "b", "region": "us-east-1", "resource_prefix": "ddb-skill-bench-a", "lambda_memory_mb": 64}"#,
            "lambda_memory_mb must be a whole number from 128 to 10240, got \"64\"",
        ),
    ];
    for (label, config, expected) in cases {
        let run = deploy_with(MODEL, config, RECORDED, &[]);
        let stderr = refused(&run);
        assert!(stderr.contains(expected), "{label}: {stderr}");
    }
}

#[test]
fn production_identities_are_refused_by_arn_or_alias() {
    for (arn, alias, markers) in [
        (
            "arn:aws:iam::111122223333:role/prod-deployer",
            "acme",
            "['prod']",
        ),
        (
            "arn:aws:iam::111122223333:user/dev",
            "acme-production",
            "['prod', 'production']",
        ),
        (
            "arn:aws:iam::111122223333:user/Live-Admin",
            "xyz",
            "['live']",
        ),
        ("arn:aws:iam::111122223333:user/dev", "mc-prd", "['prd']"),
    ] {
        let recording = format!(
            r#"{{"calls": [
              {{"service": "sts", "operation": "GetCallerIdentity", "response": {{"Account": "111122223333", "Arn": "{arn}"}}}},
              {{"service": "iam", "operation": "ListAccountAliases", "response": {{"AccountAliases": ["{alias}"]}}}}]}}"#
        );
        let run = deploy_with(MODEL, CONFIG, &recording, &[]);
        let stderr = refused(&run);
        assert!(
            stderr.starts_with(&format!(
                "error: caller identity appears to be production ({markers}). ARN: \"{arn}\"."
            )),
            "{stderr}"
        );
        assert!(
            stderr.ends_with("REFUSING to deploy benchmark resources against a prod account.\n")
        );
        assert!(
            text(&run.output.stdout).is_empty(),
            "no banner for a refused identity"
        );
    }
}

#[test]
fn review_4840_a_present_empty_or_extra_alias_entry_refuses_the_run() {
    // #4840: [""] was read as "no alias" and only the first entry was checked,
    // so a later production alias was discarded. Every entry is validated and
    // a list of more than one is ambiguous; absent and [] stay supported.
    let sts = r#"{"service": "sts", "operation": "GetCallerIdentity",
        "response": {"Account": "111122223333", "Arn": "arn:aws:iam::111122223333:user/dev"}}"#;
    for aliases in [
        r#"[""]"#,
        r#"["acme-sandbox", "acme-production"]"#,
        r#"["acme-sandbox", ""]"#,
        r#"["", "acme-sandbox"]"#,
        r#"["acme-sandbox", "Acme-Production"]"#,
    ] {
        let recording = format!(
            r#"{{"calls": [{sts},
              {{"service": "iam", "operation": "ListAccountAliases", "response": {{"AccountAliases": {aliases}}}}}]}}"#
        );
        let run = deploy_with(MODEL, CONFIG, &recording, &[]);
        let stderr = refused(&run);
        assert!(stderr.contains("AccountAliases"), "{aliases}: {stderr}");
        assert!(
            !stderr.to_lowercase().contains("acme-"),
            "{aliases}: {stderr}"
        );
        assert!(text(&run.output.stdout).is_empty(), "{aliases}");
    }
}

#[test]
fn every_failed_or_unusable_alias_lookup_refuses_the_run() {
    // #4822: the alias is part of the production proof, so a recorded service
    // error, a missing step and a malformed answer all refuse; none degrades
    // to "no alias" (the script did).
    let sts = r#"{"service": "sts", "operation": "GetCallerIdentity",
        "response": {"Account": "111122223333", "Arn": "arn:aws:iam::111122223333:user/dev"}}"#;
    for iam_step in [
        String::new(),
        r#",{"service": "iam", "operation": "ListAccountAliases",
            "error": {"code": "AccessDenied", "message": "no"}}"#
            .to_string(),
        r#",{"service": "iam", "operation": "ListAccountAliases",
            "error": {"kind": "no_credentials"}}"#
            .to_string(),
    ] {
        let recording = format!(r#"{{"calls": [{sts}{iam_step}]}}"#);
        let run = deploy_with(MODEL, CONFIG, &recording, &[]);
        let stderr = refused(&run);
        assert!(stderr.contains("list-account-aliases"), "{stderr}");
        assert!(stderr.contains("REFUSED"), "{stderr}");
        assert!(text(&run.output.stdout).is_empty());
    }
    // A present AccountAliases that is null or not a list of strings would
    // silently hide an alias from the production check: refused.
    for aliases in ["null", "\"acme\"", "[1]", "{}"] {
        let recording = format!(
            r#"{{"calls": [{sts},
              {{"service": "iam", "operation": "ListAccountAliases", "response": {{"AccountAliases": {aliases}}}}}]}}"#
        );
        let run = deploy_with(MODEL, CONFIG, &recording, &[]);
        let stderr = refused(&run);
        assert_eq!(
            stderr,
            "error: iam list-account-aliases returned an invalid response: AccountAliases must be a list of strings when present (it is "
                .to_string()
                + match aliases {
                    "null" => "null",
                    "\"acme\"" => "string",
                    "{}" => "object",
                    _ => "array",
                }
                + ")\n",
            "{aliases}"
        );
    }
}

const REVIEW_MISMATCH: &str = include_str!("data/dynamodb_deploy/review4819-mismatch.json");
const REVIEW_MISMATCH_ERROR: &str =
    include_str!("data/dynamodb_deploy/review4819-mismatch-error.json");
const REVIEW_MISSING_ARN: &str = include_str!("data/dynamodb_deploy/review4819-missing-arn.json");
const REVIEW_EMPTY_ARN: &str = include_str!("data/dynamodb_deploy/review4819-empty-arn.json");
const REVIEW_INVALID_ACCOUNT: &str =
    include_str!("data/dynamodb_deploy/review4819-invalid-account.json");
const REVIEW_SECRET_MANIFEST: &str =
    include_str!("data/dynamodb_deploy/review4819-manifest-secret-account.json");
const REVIEW_CONFIG: &str = r#"{"aws_profile":"bench","region":"us-east-1","resource_prefix":"ddb-skill-bench-review4819"}"#;
const REVIEW_MODEL: &str = r#"{"tables":[{"table_name":"Orders","key_schema":{"partition_key":"pk"}}],"access_patterns":[{"pattern_id":"P1","table":"Orders","operation":"Query","peak_rps":1}]}"#;

/// #4822: the reviewer's fixture. An IAM request that does not match the call
/// made used to be swallowed into "no alias" (exit 0, a plan with alias "").
#[test]
fn review_4822_an_iam_request_mismatch_is_fatal_not_an_empty_alias() {
    for (label, recording) in [
        ("response step", REVIEW_MISMATCH),
        ("error step", REVIEW_MISMATCH_ERROR),
    ] {
        let run = deploy_with(REVIEW_MODEL, REVIEW_CONFIG, recording, &[]);
        let stderr = refused(&run);
        assert!(stderr.contains("list-account-aliases"), "{label}: {stderr}");
        assert!(!stderr.contains("Plan written"), "{label}");
        assert!(text(&run.output.stdout).is_empty(), "{label}");
    }
    // The same recording with a matching request is refused as production.
    let matching = REVIEW_MISMATCH.replace("{\"unexpected\":true}", "{}");
    let run = deploy_with(REVIEW_MODEL, REVIEW_CONFIG, &matching, &[]);
    assert!(refused(&run).contains("appears to be production"));
}

/// #4823: the caller ARN is required, non-empty and in the caller's account,
/// even when the alias answer is empty or a recorded denial.
#[test]
fn review_4823_a_missing_empty_or_foreign_arn_is_refused() {
    let foreign = REVIEW_INVALID_ACCOUNT.replace("AKIAZZZZZZZZZZZZZZZZ", "999999999999");
    let malformed = REVIEW_MISSING_ARN.replace(
        "\"Account\":\"111122223333\"",
        "\"Account\":\"111122223333\",\"Arn\":\"arn:aws:iam::111122223333:\"",
    );
    for (label, recording) in [
        ("absent Arn", REVIEW_MISSING_ARN.to_string()),
        (
            "empty Arn with a denied alias lookup",
            REVIEW_EMPTY_ARN.to_string(),
        ),
        ("Arn of another account", foreign),
        ("Arn without a resource", malformed),
    ] {
        let run = deploy_with(REVIEW_MODEL, REVIEW_CONFIG, &recording, &[]);
        let stderr = refused(&run);
        assert!(stderr.contains("\"Arn\""), "{label}: {stderr}");
        assert!(text(&run.output.stdout).is_empty(), "{label}");
    }
}

/// #4824: an invalid recorded Account is named, never shown, on either stream,
/// and nothing is written.
#[test]
fn review_4824_an_invalid_account_is_not_echoed() {
    let run = deploy_with(REVIEW_MODEL, REVIEW_CONFIG, REVIEW_INVALID_ACCOUNT, &[]);
    let stderr = refused(&run);
    let stdout = text(&run.output.stdout);
    for stream in [&stderr, &stdout] {
        assert!(!stream.contains("AKIA"), "{stream}");
    }
    assert!(stderr.contains("\"Account\" must be a 12-digit AWS account id"));
    // A secret-shaped caller ARN and alias are not echoed either.
    let secret_arn = REVIEW_INVALID_ACCOUNT
        .replace("AKIAZZZZZZZZZZZZZZZZ", "111122223333")
        .replace("arn:aws:iam::111122223333:user/dev", "AKIAZZZZZZZZZZZZZZZZ");
    let run = deploy_with(REVIEW_MODEL, REVIEW_CONFIG, &secret_arn, &[]);
    let stderr = refused(&run);
    assert!(!stderr.contains("AKIA") && !text(&run.output.stdout).contains("AKIA"));
    let secret_alias = REVIEW_INVALID_ACCOUNT
        .replace("AKIAZZZZZZZZZZZZZZZZ", "111122223333")
        .replace(
            "\"AccountAliases\":[]",
            "\"AccountAliases\":[\"AKIA/secret key\"]",
        );
    let run = deploy_with(REVIEW_MODEL, REVIEW_CONFIG, &secret_alias, &[]);
    let stderr = refused(&run);
    assert!(!stderr.contains("AKIA") && !text(&run.output.stdout).contains("AKIA"));
}

/// #4824: the teardown manifest takes the same rule for its account, and every
/// other rejected manifest value is named, not shown.
#[test]
fn review_4824_manifest_values_are_not_echoed() {
    let secret = "AKIAZZZZZZZZZZZZZZZZ";
    let mut cases = vec![("account", REVIEW_SECRET_MANIFEST.to_string())];
    for (label, from, to) in [
        (
            "prefix",
            "\"ddb-skill-bench-r1-\"",
            "\"AKIAZZZZZZZZZZZZZZZZ\"",
        ),
        (
            "prefix charset",
            "\"ddb-skill-bench-r1-\"",
            "\"ddb-skill-bench-AKIA*\"",
        ),
        ("table", "ddb-skill-bench-r1-Orders", "AKIAZZZZZZZZZZZZZZZZ"),
        (
            "table charset",
            "ddb-skill-bench-r1-Orders",
            "ddb-skill-bench-r1-AKIA$(x)",
        ),
        ("region", "us-east-1", "AKIAZZZZZZZZZZZZZZZZ"),
        ("run_id", "\"r1\"", "\"AKIA\\u001b[2J\""),
        (
            "function",
            "\"lambda\":{}",
            "\"lambda\":{\"function_name\":\"AKIAZZZZ\"}",
        ),
        (
            "role",
            "\"lambda\":{}",
            "\"lambda\":{\"function_name\":\"ddb-skill-bench-r1-f\",\"role_name\":\"AKIAZZZZ\"}",
        ),
        (
            "policy",
            "\"lambda\":{}",
            "\"lambda\":{\"policy_name\":\"AKIA'ZZZZ\"}",
        ),
    ] {
        let base = REVIEW_SECRET_MANIFEST.replace(secret, "111122223333");
        cases.push((label, base.replace(from, to)));
    }
    for (label, manifest) in cases {
        let dir = temp_root("teardown-secret");
        let output = teardown_in(&dir, &manifest, &["--yes-generate-teardown"]);
        assert_eq!(status(&output), 2, "{label}: {}", text(&output.stderr));
        for stream in [text(&output.stderr), text(&output.stdout)] {
            assert!(!stream.contains("AKIA"), "{label}: {stream}");
        }
        assert!(
            !dir.join("teardown.sh").exists(),
            "{label}: nothing written"
        );
    }
}

#[test]
fn sts_failures_keep_the_scripts_messages() {
    for (error, expected) in [
        (r#"{"kind": "no_credentials"}"#, "error: AWS credentials not found for profile \"bench\". Try: aws sso login --profile \"bench\"\n"),
        (
            r#"{"code": "ExpiredToken", "message": "token expired"}"#,
            "error: sts get-caller-identity failed (\"ExpiredToken\"): \"token expired\". Credentials may be expired — try: aws sso login --profile \"bench\"\n",
        ),
        (r#"{"kind": "connect", "message": "dns"}"#, "error: cannot reach AWS endpoint: "),
    ] {
        let recording = format!(
            r#"{{"calls": [{{"service": "sts", "operation": "GetCallerIdentity", "error": {error}}}]}}"#
        );
        let run = deploy_with(MODEL, CONFIG, &recording, &[]);
        let stderr = refused(&run);
        assert!(stderr.starts_with(expected), "{stderr}");
    }
}

#[test]
fn the_caller_account_must_be_twelve_digits_before_it_reaches_a_trust_policy() {
    for account in ["", "1111", "11112222333a", "1111222233334"] {
        let recording = format!(
            r#"{{"calls": [{{"service": "sts", "operation": "GetCallerIdentity",
              "response": {{"Account": "{account}", "Arn": "arn:aws:iam::1:user/u"}}}}]}}"#
        );
        let run = deploy_with(MODEL, CONFIG, &recording, &[]);
        let stderr = refused(&run);
        assert!(
            stderr.contains("\"Account\" must be a 12-digit AWS account id"),
            "{account}: {stderr}"
        );
        assert!(account.is_empty() || !stderr.contains(account), "{stderr}");
    }
}

#[test]
fn malformed_recordings_are_rejected_not_defaulted() {
    let ok_iam = r#"{"service": "iam", "operation": "ListAccountAliases", "response": {}}"#;
    let ok_sts = r#"{"service": "sts", "operation": "GetCallerIdentity",
        "response": {"Account": "111122223333", "Arn": "arn:aws:iam::111122223333:user/dev"}}"#;
    let cases: [(&str, String, &str); 14] = [
        ("no calls", "{}".to_string(), "needs a \"calls\" array"),
        ("calls null", r#"{"calls": null}"#.to_string(), "\"calls\" must be a list when present (it is null)"),
        ("request null", r#"{"calls": [{"service": "sts", "operation": "GetCallerIdentity", "request": null, "response": {}}]}"#.to_string(), "recorded call 0: \"request\" must be an object when present (it is null)"),
        ("both outcomes", r#"{"calls": [{"service": "sts", "operation": "GetCallerIdentity", "response": {}, "error": {}}]}"#.to_string(), "exactly one of \"response\" (an object) and \"error\" (an object) is required"),
        ("response null", r#"{"calls": [{"service": "sts", "operation": "GetCallerIdentity", "response": null}]}"#.to_string(), "exactly one of"),
        ("unknown op", r#"{"calls": [{"service": "sts", "operation": "AssumeRole", "response": {}}]}"#.to_string(), "must be sts GetCallerIdentity or iam ListAccountAliases"),
        ("service number", r#"{"calls": [{"service": 5, "operation": "x", "response": {}}]}"#.to_string(), "\"service\" must be a string when present (it is integer)"),
        ("error kind", r#"{"calls": [{"service": "sts", "operation": "GetCallerIdentity", "error": {"kind": "other"}}]}"#.to_string(), "\"error.kind\" must be client, connect or no_credentials"),
        ("error code null", r#"{"calls": [{"service": "sts", "operation": "GetCallerIdentity", "error": {"code": null}}]}"#.to_string(), "error \"code\" must be a string when present (it is null)"),
        ("error message number", r#"{"calls": [{"service": "sts", "operation": "GetCallerIdentity", "error": {"message": 3}}]}"#.to_string(), "error \"message\" must be a string when present (it is integer)"),
        ("arn null", r#"{"calls": [{"service": "sts", "operation": "GetCallerIdentity", "response": {"Account": "111122223333", "Arn": null}}]}"#.to_string(), "sts get-caller-identity returned an invalid response: Arn must be a string when present (it is null)"),
        ("account number", r#"{"calls": [{"service": "sts", "operation": "GetCallerIdentity", "response": {"Account": 111122223333}}]}"#.to_string(), "Account must be a string when present (it is integer)"),
        (
            "request mismatch on a failure step",
            r#"{"calls": [{"service": "sts", "operation": "GetCallerIdentity", "request": {"x": 1},
               "error": {"code": "Denied", "message": "no"}}]}"#.to_string(),
            "cannot reach AWS endpoint: ",
        ),
        (
            "unused step",
            format!(r#"{{"calls": [{ok_sts}, {ok_iam}, {ok_sts}]}}"#),
            "the recorded document has 1 step(s) this run never used",
        ),
    ];
    for (label, recording, expected) in cases {
        let run = deploy_with(MODEL, CONFIG, &recording, &[]);
        let stderr = refused(&run);
        assert!(stderr.contains(expected), "{label}: {stderr}");
    }
}

#[test]
fn a_recorded_request_that_differs_from_the_call_made_is_refused_for_responses_too() {
    let recording = r#"{"calls": [{"service": "sts", "operation": "GetCallerIdentity",
        "request": {"unexpected": true}, "response": {"Account": "111122223333", "Arn": "arn:aws:iam::111122223333:user/dev"}}]}"#;
    let run = deploy_with(MODEL, CONFIG, recording, &[]);
    let stderr = refused(&run);
    assert!(
        stderr.contains(
            "the recorded request of sts:GetCallerIdentity differs from the request made"
        ),
        "{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn the_plan_is_private_atomic_and_never_written_through_a_link() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let run = deploy();
    let plan = run.dir.join("plan.json");
    assert_eq!(
        fs::metadata(&plan).expect("plan").permissions().mode() & 0o777,
        0o600
    );
    let leftovers: Vec<_> = fs::read_dir(&*run.dir)
        .expect("dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.') && n.contains(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");

    // A second run replaces the regular file and keeps it private.
    fs::set_permissions(&plan, fs::Permissions::from_mode(0o644)).expect("chmod");
    let again = toolkit(
        &[
            "dynamodb",
            "deploy-model",
            "--model",
            "model.json",
            "--config",
            "config.json",
            "--manifest-out",
            "created_resources.json",
            "--yes-deploy",
            "--recorded",
            "rec.json",
            "--plan-out",
            "plan.json",
            "--run-id",
            RUN_ID,
        ],
        &run.dir,
    );
    assert_eq!(status(&again), 0, "{}", text(&again.stderr));
    assert_eq!(
        fs::metadata(&plan).expect("plan").permissions().mode() & 0o777,
        0o600
    );

    // A symbolic link at the output is refused and its target is untouched.
    let target = run.dir.join("victim.txt");
    fs::write(&target, "keep").expect("victim");
    let link = run.dir.join("link.json");
    symlink(&target, &link).expect("symlink");
    let linked = toolkit(
        &[
            "dynamodb",
            "deploy-model",
            "--model",
            "model.json",
            "--config",
            "config.json",
            "--manifest-out",
            "created_resources.json",
            "--yes-deploy",
            "--recorded",
            "rec.json",
            "--plan-out",
            "link.json",
        ],
        &run.dir,
    );
    assert_eq!(status(&linked), 2);
    assert!(
        text(&linked.stderr).contains("symbolic links are not followed"),
        "{}",
        text(&linked.stderr)
    );
    assert_eq!(fs::read_to_string(&target).expect("victim"), "keep");

    // A directory at the output is refused.
    fs::create_dir(run.dir.join("outdir")).expect("dir");
    let dir_out = toolkit(
        &[
            "dynamodb",
            "deploy-model",
            "--model",
            "model.json",
            "--config",
            "config.json",
            "--manifest-out",
            "created_resources.json",
            "--yes-deploy",
            "--recorded",
            "rec.json",
            "--plan-out",
            "outdir",
        ],
        &run.dir,
    );
    assert_eq!(status(&dir_out), 2);
    assert!(text(&dir_out.stderr).contains("not a regular file"));
}

#[cfg(unix)]
#[test]
fn inputs_that_are_symbolic_links_are_refused() {
    use std::os::unix::fs::symlink;

    let run = deploy();
    fs::remove_file(run.dir.join("plan.json")).expect("rm");
    fs::rename(run.dir.join("model.json"), run.dir.join("real.json")).expect("mv");
    symlink(run.dir.join("real.json"), run.dir.join("model.json")).expect("link");
    let again = toolkit(
        &[
            "dynamodb",
            "deploy-model",
            "--model",
            "model.json",
            "--config",
            "config.json",
            "--manifest-out",
            "created_resources.json",
            "--yes-deploy",
            "--recorded",
            "rec.json",
            "--plan-out",
            "plan.json",
        ],
        &run.dir,
    );
    assert_eq!(status(&again), 2);
    assert!(
        text(&again.stderr)
            .contains("cannot read the design JSON model.json: symbolic links are not followed"),
        "{}",
        text(&again.stderr)
    );
}

#[test]
fn run_overrides_are_validated() {
    for (flag, value) in [
        ("--created-at", "2026-01-31"),
        ("--created-at", "2026-01-31T12:00:00+00:00"),
        ("--run-id", "XYZ"),
        ("--run-id", "0123456789ABCDEF0123456789ABCDEF"),
    ] {
        let run = deploy_with(MODEL, CONFIG, RECORDED, &[flag, value]);
        // The later occurrence wins (last value flag), so the bad one is used.
        let stderr = refused(&run);
        assert!(stderr.contains(&format!("{flag} must")), "{flag}: {stderr}");
    }
}

// ---------------------------------------------------------------------------
// generate-teardown
// ---------------------------------------------------------------------------

const MANIFEST: &str = r#"{
  "account": "111122223333", "alias": "acme-sandbox", "aws_profile": "bench", "region": "us-east-1",
  "run_id": "0123456789abcdef0123456789abcdef", "created_at": "2026-01-31T12:00:00Z",
  "prefix": "ddb-skill-bench-test-",
  "tables": [{"name": "ddb-skill-bench-test-Orders", "original_name": "Orders", "arn": null},
             {"name": "ddb-skill-bench-test-Docs"}],
  "lambda": {"function_name": "ddb-skill-bench-test-bench", "role_name": "ddb-skill-bench-test-bench-role",
             "policy_name": "ddb-skill-bench-test-bench-policy"}
}"#;

fn teardown_in(dir: &Path, manifest: &str, extra: &[&str]) -> Output {
    fs::write(dir.join("created_resources.json"), manifest).expect("manifest");
    let mut args = vec![
        "dynamodb",
        "generate-teardown",
        "--manifest",
        "created_resources.json",
        "--out",
        "teardown.sh",
    ];
    args.extend_from_slice(extra);
    toolkit(&args, dir)
}

#[test]
fn teardown_generation_requires_the_confirmation_flag_and_writes_nothing_without_it() {
    let dir = temp_root("teardown-consent");
    let output = teardown_in(&dir, MANIFEST, &[]);
    assert_eq!(status(&output), 2);
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with(
            "error: refusing to generate a teardown script without --yes-generate-teardown."
        ),
        "{stderr}"
    );
    assert!(stderr.ends_with("Nothing was written.\n"), "{stderr}");
    assert!(!dir.join("teardown.sh").exists());
    // The consent check precedes reading the manifest: a missing one is not read.
    let missing = toolkit(
        &[
            "dynamodb",
            "generate-teardown",
            "--manifest",
            "nope.json",
            "--out",
            "teardown.sh",
        ],
        &dir,
    );
    assert!(text(&missing.stderr).starts_with("error: refusing to generate a teardown script"));
    assert!(!dir.join("teardown.sh").exists());
    // `--yes-deploy` is not consent to generate a deletion script.
    let wrong = teardown_in(&dir, MANIFEST, &["--yes-deploy"]);
    assert_eq!(status(&wrong), 2);
    assert!(!dir.join("teardown.sh").exists());
}

#[test]
fn the_teardown_script_has_the_scripts_assignments_and_guards() {
    let dir = temp_root("teardown-render");
    let output = teardown_in(&dir, MANIFEST, &["--yes-generate-teardown"]);
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        "Teardown script written to: teardown.sh\n\nReview the script, then:\n\
         \x20 bash teardown.sh --dry-run   # preview — calls no delete APIs\n\
         \x20 bash teardown.sh --confirm   # delete the tables, Lambda, and IAM role\n\
         \x20 bash teardown.sh --confirm --delete-logs   # also delete the Lambda's CloudWatch log group (its run logs are gone for good)\n"
    );
    let script = fs::read_to_string(dir.join("teardown.sh")).expect("script");
    for line in [
        "MANIFEST_ACCOUNT='111122223333'",
        "MANIFEST_REGION='us-east-1'",
        "MANIFEST_PREFIX='ddb-skill-bench-test-'",
        "MANIFEST_RUN_ID='0123456789abcdef0123456789abcdef'",
        "AWS_PROFILE_HINT='bench'",
        "TABLES=('ddb-skill-bench-test-Orders' 'ddb-skill-bench-test-Docs')",
        "LAMBDA_FN='ddb-skill-bench-test-bench'",
        "LAMBDA_ROLE='ddb-skill-bench-test-bench-role'",
        "LAMBDA_POLICY='ddb-skill-bench-test-bench-policy'",
        "LAMBDA_LOG_GROUP='/aws/lambda/ddb-skill-bench-test-bench'",
        "set -euo pipefail",
        "caller_account=$(aws sts get-caller-identity --region \"$MANIFEST_REGION\" --query Account --output text 2>&1) || {",
        "if [[ $CONFIRM -ne 1 ]]; then",
        "    \"$MANIFEST_PREFIX\"*) ;;",
        "    /aws/lambda/\"$MANIFEST_PREFIX\"*) ;;",
    ] {
        assert!(script.lines().any(|l| l == line), "missing line {line:?}");
    }
    assert!(!script.contains("python"), "the script needs no Python");
    assert!(!script.contains("%%"), "every marker is filled");
    // Deletion commands exist only behind the confirm/dry-run branches.
    assert_eq!(script.matches("aws dynamodb delete-table").count(), 1);
    assert_eq!(script.matches("aws lambda delete-function").count(), 1);
    assert_eq!(script.matches("aws iam delete-role ").count(), 1);
    assert_eq!(script.matches("aws iam delete-role-policy").count(), 1);
    assert_eq!(script.matches("aws logs delete-log-group").count(), 1);
}

#[cfg(unix)]
#[test]
fn the_script_is_owner_only_executable_and_runs_to_its_confirm_refusal() {
    use std::os::unix::fs::PermissionsExt;

    let dir = temp_root("teardown-run");
    let output = teardown_in(&dir, MANIFEST, &["--yes-generate-teardown"]);
    assert_eq!(status(&output), 0);
    let script = dir.join("teardown.sh");
    assert_eq!(
        fs::metadata(&script).expect("script").permissions().mode() & 0o777,
        0o700
    );
    let Ok(syntax) = Command::new("bash")
        .arg("-n")
        .arg(&script)
        .env_clear()
        .output()
    else {
        return; // no bash on this machine: the structural assertions above stand
    };
    assert!(syntax.status.success(), "{}", text(&syntax.stderr));
    // Without --confirm the script prints its banner and refuses before any
    // `aws` call (PATH is empty, so an attempted call could not succeed).
    let run = Command::new("bash")
        .arg(&script)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("run");
    assert_eq!(status(&run), 2);
    let stdout = text(&run.stdout);
    assert!(
        stdout.contains("  Account expected (from manifest): 111122223333\n"),
        "{stdout}"
    );
    assert!(stdout.contains("    - ddb-skill-bench-test-Orders\n"));
    assert!(text(&run.stderr).starts_with("Refusing to run without --confirm.\n"));
}

#[cfg(unix)]
#[test]
fn hostile_manifest_text_is_quoted_not_executed() {
    let dir = temp_root("teardown-hostile");
    let manifest = MANIFEST.replace(
        "\"aws_profile\": \"bench\"",
        "\"aws_profile\": \"x'; touch PWNED; echo '\"",
    );
    let output = teardown_in(&dir, &manifest, &["--yes-generate-teardown"]);
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    let script = fs::read_to_string(dir.join("teardown.sh")).expect("script");
    assert!(
        script.contains("AWS_PROFILE_HINT='x'\\''; touch PWNED; echo '\\'''\n"),
        "{script}"
    );
    if let Ok(run) = Command::new("bash")
        .arg(dir.join("teardown.sh"))
        .current_dir(&*dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
    {
        assert_eq!(status(&run), 2);
        assert!(
            !dir.join("PWNED").exists(),
            "manifest text must never execute"
        );
    }
}

#[test]
fn manifests_that_do_not_belong_to_one_run_prefix_are_refused() {
    let cases: [(&str, String, &str); 14] = [
        ("no prefix", MANIFEST.replace("\"prefix\": \"ddb-skill-bench-test-\",", ""), "manifest \"prefix\" is missing, is not a string or does not start with 'ddb-skill-bench-'. Refusing to generate teardown."),
        ("foreign prefix", MANIFEST.replace("\"prefix\": \"ddb-skill-bench-test-\"", "\"prefix\": \"prod-\""), "manifest \"prefix\" is missing, is not a string or does not start with"),
        ("null prefix", MANIFEST.replace("\"prefix\": \"ddb-skill-bench-test-\"", "\"prefix\": null"), "manifest \"prefix\" is missing, is not a string"),
        ("table outside the prefix", MANIFEST.replace("ddb-skill-bench-test-Docs", "Users"), "manifest tables[].name: 1 name(s) do not start with the manifest prefix. Refusing to generate teardown."),
        ("lambda outside the prefix", MANIFEST.replace("\"function_name\": \"ddb-skill-bench-test-bench\"", "\"function_name\": \"other\""), "manifest lambda function_name does not start with the manifest prefix"),
        ("role outside the prefix", MANIFEST.replace("\"role_name\": \"ddb-skill-bench-test-bench-role\"", "\"role_name\": \"admin\""), "manifest lambda role_name does not start with the manifest prefix"),
        ("shell in a table name", MANIFEST.replace("ddb-skill-bench-test-Docs", "ddb-skill-bench-test-$(id)"), "manifest tables[].name must be 1 to 255 characters"),
        ("wildcard prefix", MANIFEST.replace("\"prefix\": \"ddb-skill-bench-test-\"", "\"prefix\": \"ddb-skill-bench-*\""), "must be at most 48 characters of letters, digits, '-' and '_'"),
        ("no account", MANIFEST.replace("\"account\": \"111122223333\",", ""), "manifest \"account\" must be a 12-digit AWS account id."),
        ("bad region", MANIFEST.replace("us-east-1", "us east 1"), "is not an AWS region name"),
        ("tables wrong type", MANIFEST.replace("\"tables\": [{\"name\": \"ddb-skill-bench-test-Orders\", \"original_name\": \"Orders\", \"arn\": null},\n             {\"name\": \"ddb-skill-bench-test-Docs\"}]", "\"tables\": \"x\""), "\"tables\" must be an array (it is string)"),
        ("lambda wrong type", MANIFEST.replace("\"lambda\": {", "\"lambda\": [1], \"x\": {"), "\"lambda\" must be an object (it is array)"),
        ("policy with a quote", MANIFEST.replace("ddb-skill-bench-test-bench-policy", "p'q"), "policy name must be 1 to 128 characters"),
        ("not an object", "[]".to_string(), "the manifest must be a JSON object"),
    ];
    for (label, manifest, expected) in cases {
        let dir = temp_root("teardown-refuse");
        let output = teardown_in(&dir, &manifest, &["--yes-generate-teardown"]);
        assert_eq!(status(&output), 2, "{label}: {}", text(&output.stderr));
        assert!(
            text(&output.stderr).contains(expected),
            "{label}: {}",
            text(&output.stderr)
        );
        assert!(
            !dir.join("teardown.sh").exists(),
            "{label}: nothing written"
        );
    }
}

#[test]
fn null_lambda_members_mean_not_set_and_a_manifest_without_lambda_is_valid() {
    let dir = temp_root("teardown-nolambda");
    let manifest = MANIFEST.replace(
        "\"lambda\": {\"function_name\": \"ddb-skill-bench-test-bench\", \"role_name\": \"ddb-skill-bench-test-bench-role\",\n             \"policy_name\": \"ddb-skill-bench-test-bench-policy\"}",
        "\"lambda\": {\"function_name\": null, \"role_name\": null}",
    );
    let output = teardown_in(&dir, &manifest, &["--yes-generate-teardown"]);
    assert_eq!(status(&output), 0, "{}", text(&output.stderr));
    let script = fs::read_to_string(dir.join("teardown.sh")).expect("script");
    assert!(script.lines().any(|l| l == "LAMBDA_FN=''"));
    assert!(script.lines().any(|l| l == "LAMBDA_LOG_GROUP=''"));
}

#[test]
fn missing_and_linked_manifests_and_outputs_are_refused() {
    let dir = temp_root("teardown-paths");
    let missing = toolkit(
        &[
            "dynamodb",
            "generate-teardown",
            "--manifest",
            "nope.json",
            "--out",
            "teardown.sh",
            "--yes-generate-teardown",
        ],
        &dir,
    );
    assert_eq!(status(&missing), 2);
    assert_eq!(
        text(&missing.stderr),
        "error: manifest not found: nope.json\n"
    );
    // --out must not overwrite the manifest.
    fs::write(dir.join("m.json"), MANIFEST).expect("manifest");
    let same = toolkit(
        &[
            "dynamodb",
            "generate-teardown",
            "--manifest",
            "m.json",
            "--out",
            "./m.json",
            "--yes-generate-teardown",
        ],
        &dir,
    );
    assert_eq!(status(&same), 2);
    assert!(text(&same.stderr).contains("--out would overwrite the manifest"));
    assert_eq!(
        fs::read_to_string(dir.join("m.json")).expect("manifest"),
        MANIFEST
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(dir.join("m.json"), dir.join("linked.json")).expect("link");
        let linked = toolkit(
            &[
                "dynamodb",
                "generate-teardown",
                "--manifest",
                "linked.json",
                "--out",
                "t.sh",
                "--yes-generate-teardown",
            ],
            &dir,
        );
        assert_eq!(status(&linked), 2);
        assert!(text(&linked.stderr).contains("symbolic links are not followed"));
        let victim = dir.join("victim");
        fs::write(&victim, "keep").expect("victim");
        symlink(&victim, dir.join("out.sh")).expect("link");
        let out_link = toolkit(
            &[
                "dynamodb",
                "generate-teardown",
                "--manifest",
                "m.json",
                "--out",
                "out.sh",
                "--yes-generate-teardown",
            ],
            &dir,
        );
        assert_eq!(status(&out_link), 2);
        assert_eq!(fs::read_to_string(&victim).expect("victim"), "keep");
    }
}

#[test]
fn hostile_paths_and_values_never_reach_the_terminal_raw() {
    let dir = temp_root("hostile");
    let hostile = "bad\u{1b}[31m\u{202e}name.json";
    let output = toolkit(
        &[
            "dynamodb",
            "generate-teardown",
            "--manifest",
            hostile,
            "--out",
            "t.sh",
            "--yes-generate-teardown",
        ],
        &dir,
    );
    assert_eq!(status(&output), 2);
    let stderr = text(&output.stderr);
    assert!(
        !stderr.contains('\u{1b}') && !stderr.contains('\u{202e}'),
        "{stderr:?}"
    );
    // A hostile region in a manifest is quoted in the refusal.
    let manifest = MANIFEST.replace("us-east-1", "us-east-1\u{1b}[31m");
    let refused = teardown_in(&dir, &manifest, &["--yes-generate-teardown"]);
    assert_eq!(status(&refused), 2);
    assert!(
        !text(&refused.stderr).contains('\u{1b}'),
        "{:?}",
        text(&refused.stderr)
    );
}

/// #4824: a recorded IAM error with credential-shaped text is redacted by the
/// shared AWS error type before it reaches the diagnostic.
#[test]
fn a_credential_shaped_iam_error_is_redacted() {
    let recording = r#"{"calls": [{"service": "sts", "operation": "GetCallerIdentity",
        "response": {"Account": "111122223333", "Arn": "arn:aws:iam::111122223333:user/dev"}},
      {"service": "iam", "operation": "ListAccountAliases",
       "error": {"code": "AKIAZZZZZZZZZZZZZZZZ", "message": "Authorization: AWS4 AKIAZZZZZZ"}}]}"#;
    let run = deploy_with(MODEL, CONFIG, recording, &[]);
    let stderr = refused(&run);
    assert!(stderr.contains("[REDACTED]"), "{stderr}");
    assert!(!stderr.contains("AKIA"), "{stderr}");
}
