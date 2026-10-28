//! `neptune validate-input` and `neptune seed`.

use super::{json, run, TempDir, SECRET};
use crate::docgraph::neptune_input::{valid_keys_repr, validate_all};
use crate::docgraph::seed::{
    dry_run_lines, phases, GremlinSession, NoLiveSession, RecordedSession, SeedError, Statement,
    WIPE, WIPE_REFUSAL,
};
use toolkit_core::Json;

fn errors(pairs: &[(&str, &str)]) -> Vec<String> {
    let pairs: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    validate_all(&pairs)
}

fn valid(key: &str, value: &str) -> bool {
    errors(&[(key, value)]).is_empty()
}

#[test]
fn the_documented_examples_validate() {
    assert_eq!(
        errors(&[
            ("cluster_id", "prod-graph"),
            ("region", "us-east-1"),
            ("vpc_id", "vpc-0abc12345def67890"),
        ]),
        Vec::<String>::new()
    );
    assert!(valid("graph_id", "g-abc123def4"));
    assert!(valid(
        "kms_key",
        "arn:aws:kms:us-east-1:123456789012:key/1234abcd-12ab-34cd-56ef-1234567890ab"
    ));
    assert!(valid("kms_key", "1234abcd-12ab-34cd-56ef-1234567890ab"));
    assert!(valid(
        "kms_key",
        "arn:aws-cn:kms:cn-north-1:123456789012:key/ab-cd"
    ));
    assert!(valid(
        "kms_key",
        "arn:aws-us-gov:kms:us-gov-west-1:123456789012:key/ab-cd"
    ));
    assert!(valid("iam_role", "arn:aws:iam::123456789012:role/Role"));
    assert!(valid(
        "iam_role",
        "arn:aws-cn:iam::123456789012:role/service-role/Role"
    ));
    assert!(valid("s3_uri", "s3://my-bucket"));
    assert!(valid("s3_uri", "s3://my-bucket/path/to/data/"));
    assert!(valid("snapshot_id", "snap-1"));
    assert!(valid("provisioned_memory", "16"));
    assert!(valid("provisioned_memory", "24576"));
    assert!(valid("provisioned_memory", " 128 "));
    assert!(valid("subnet_ids", "subnet-0abc12345, subnet-0def67890"));
    assert!(valid("security_group_ids", "sg-0abc12345"));
}

#[test]
fn identifier_boundaries_follow_the_patterns() {
    let long = |n: usize| format!("a{}", "b".repeat(n - 1));
    assert!(valid("cluster_id", &long(63)));
    assert!(!valid("cluster_id", &long(64)));
    assert!(valid("snapshot_id", &long(255)));
    assert!(!valid("snapshot_id", &long(256)));
    for bad in [
        "", "1abc", "-abc", "ab_c", "ab.c", "ab c", "ab\n", "\nab", "abç",
    ] {
        assert!(!valid("cluster_id", bad), "{bad:?}");
        assert!(!valid("snapshot_id", bad), "{bad:?}");
    }
    assert!(valid("graph_id", &format!("g-{}", "a1".repeat(5))));
    assert!(!valid("graph_id", "g-abc12345"), "nine characters");
    assert!(!valid("graph_id", "g-ABC123DEF4"));
    assert!(!valid("graph_id", "abc123def4"));
    for (value, ok) in [
        ("vpc-12345678", true),
        ("vpc-1234567", false),
        ("vpc-0123456789abcdef0", true),
        ("vpc-0123456789abcdef01", false),
        ("vpc-1234567G", false),
        ("vpc-1234567\n", false),
        ("sg-12345678", false),
    ] {
        assert_eq!(valid("vpc_id", value), ok, "{value:?}");
    }
    for (value, ok) in [
        ("us-east-1", true),
        ("eu-west-2", true),
        ("ap-southeast-4", true),
        ("us-gov-west-1", true),
        ("us-east-", false),
        ("US-east-1", false),
        ("us-east-1\n", false),
        ("useast1", false),
        ("us-1", false),
    ] {
        assert_eq!(valid("region", value), ok, "{value:?}");
    }
    for (value, ok) in [
        ("provisioned_memory 15", false),
        ("16", true),
        ("24577", false),
        ("abc", false),
        ("", false),
        ("1.5", false),
    ] {
        let value = value.trim_start_matches("provisioned_memory ");
        assert_eq!(valid("provisioned_memory", value), ok, "{value:?}");
    }
}

#[test]
fn arn_and_uri_forms_are_exact() {
    for bad in [
        "arn:aws:kms:us-east-1:12345678901:key/abc",
        "arn:aws:kms:us-east-1:123456789012:key/",
        "arn:aws:kms:us-east-1:123456789012:alias/x",
        "arn:aws:kms::123456789012:key/ab",
        "arn:aws:kms:us-east-1:123456789012:key/ab:cd",
        "arn:aws-eu:kms:us-east-1:123456789012:key/ab",
        "1234abcd-12ab-34cd-56ef-1234567890a",
        "1234abcd-12ab-34cd-56ef-1234567890ab\n",
        "1234ABCD-12ab-34cd-56ef-1234567890ab",
    ] {
        assert!(!valid("kms_key", bad), "{bad:?}");
    }
    for bad in [
        "arn:aws:iam::123456789012:role/",
        "arn:aws:iam::12345678901:role/x",
        "arn:aws:iam::123456789012:user/x",
        "arn:aws:iam::123456789012:role/x\ny",
        "arn:aws:iam:us-east-1:123456789012:role/x",
        "role/x",
    ] {
        assert!(!valid("iam_role", bad), "{bad:?}");
    }
    for bad in [
        "s3://ab",
        "s3://-bucket",
        "s3://bucket-",
        "s3://Bucket",
        "s3://bu_cket",
        "s3://bucket/",
        "s3://bucket/x\ny",
        "s3:/bucket",
        "http://bucket",
        &format!("s3://{}", "a".repeat(64)),
    ] {
        assert!(!valid("s3_uri", bad), "{bad:?}");
    }
    assert!(valid("s3_uri", &format!("s3://{}", "a".repeat(63))));
    assert!(valid("s3_uri", "s3://a.b-c/x y"));
}

#[test]
fn messages_are_those_of_the_script_and_lists_report_each_item() {
    assert_eq!(
        errors(&[("cluster_id", "1bad")]),
        ["cluster_id '1bad' invalid. Must start with letter, alphanumeric + hyphens, max 63 chars."]
    );
    assert_eq!(
        errors(&[("graph_id", "x")]),
        ["graph_id 'x' invalid. Must match 'g-' followed by lowercase alphanumeric (e.g., g-abc123def4)."]
    );
    assert_eq!(
        errors(&[("provisioned_memory", "abc")]),
        ["provisioned_memory 'abc' invalid. Must be an integer."]
    );
    assert_eq!(
        errors(&[("provisioned_memory", "+12")]),
        ["provisioned_memory 12 invalid. Valid range: 16-24576 m-NCU."]
    );
    assert_eq!(
        errors(&[("subnet_ids", "subnet-0abc12345,bad, subnet-0def67890,")]),
        [
            "subnet_id 'bad' invalid. Expected format: subnet-0abc12345def67890.",
            "subnet_id '' invalid. Expected format: subnet-0abc12345def67890."
        ]
    );
    assert_eq!(
        errors(&[("security_group_ids", "sg-1")]),
        ["security_group_id 'sg-1' invalid. Expected format: sg-0abc12345def67890."]
    );
    assert_eq!(
        errors(&[("flavor", "x"), ("region", "nope")]),
        [
            "Unknown validator key: 'flavor'. Valid keys: ['cluster_id', 'graph_id', 'iam_role', 'kms_key', 'provisioned_memory', 'region', 's3_uri', 'security_group_ids', 'snapshot_id', 'subnet_ids', 'vpc_id']",
            "region 'nope' invalid. Expected format: us-east-1, eu-west-2, etc."
        ]
    );
    assert!(valid_keys_repr().starts_with("['cluster_id', 'graph_id'"));
}

#[test]
fn shown_values_are_escaped_bounded_and_redacted() {
    let hostile = format!("a\u{1b}[31m\u{202e}\nerror: injected {SECRET}");
    let message = &errors(&[("cluster_id", &hostile)])[0];
    assert!(
        !message.chars().any(|c| c.is_control() || c == '\u{202e}'),
        "{message:?}"
    );
    assert!(!message.contains(SECRET));
    let long = "x".repeat(5000);
    assert!(errors(&[("snapshot_id", &format!("1{long}"))])[0].len() < 600);
}

#[test]
fn the_command_prints_usage_results_and_exit_codes() {
    let ran = run(&["neptune", "validate-input"]);
    assert_eq!(ran.status, 0);
    assert_eq!(
        ran.stdout,
        format!(
            "Usage: toolkit neptune validate-input key=value [key=value ...]\nValid keys: {}\n",
            valid_keys_repr()
        )
    );
    let ran = run(&[
        "neptune",
        "validate-input",
        "cluster_id=prod-graph",
        "region=us-east-1",
    ]);
    assert_eq!(
        (ran.status, ran.stdout.as_str()),
        (0, "\u{2705} All validations passed.\n")
    );
    let ran = run(&["neptune", "validate-input", "cluster_id=1x", "vpc_id=vpc-1"]);
    assert_eq!(ran.status, 1);
    assert!(ran
        .stdout
        .starts_with("Validation failed:\n  \u{274c} cluster_id '1x' invalid."));
    assert_eq!(ran.stdout.lines().count(), 3);
    // The last duplicate wins; a missing '=' stops at once.
    let ran = run(&[
        "neptune",
        "validate-input",
        "region=bad",
        "region=us-east-1",
    ]);
    assert_eq!(ran.status, 0, "{}", ran.stdout);
    let ran = run(&["neptune", "validate-input", "region=bad", "oops"]);
    assert_eq!(
        (ran.status, ran.stdout.as_str()),
        (1, "Invalid argument: oops. Use key=value format.\n")
    );
}

// ------------------------------------------------------------------- seed

/// A recording of the whole run: one entry per statement in order, with the
/// answers the verification needs.
fn recording(answers: &[(&str, &str)]) -> String {
    let mut steps: Vec<(Json, String)> = vec![(
        Statement {
            gremlin: WIPE.to_string(),
            bindings: None,
        }
        .request(),
        r#"{"results": []}"#.to_string(),
    )];
    for phase in phases() {
        for statement in &phase.statements {
            steps.push((statement.request(), r#"{"results": []}"#.to_string()));
        }
    }
    let queries = [
        ("g.V().count()", r#"{"results": [12]}"#),
        ("g.E().count()", r#"{"results": [14]}"#),
        (
            "g.V().has('Account','id','A2').out('USES').in('USES').dedup().values('id').toList()",
            r#"{"results": ["A1", "A2", "A3"]}"#,
        ),
        (
            "g.V().has('Person','id','U1').out('FOLLOWS').out('FOLLOWS').dedup().values('name').toList()",
            r#"{"results": ["Dave"]}"#,
        ),
    ];
    for (query, response) in queries {
        let response = answers
            .iter()
            .find(|(q, _)| *q == query)
            .map_or(response, |(_, r)| r);
        steps.push((
            Statement {
                gremlin: query.to_string(),
                bindings: None,
            }
            .request(),
            response.to_string(),
        ));
    }
    let calls: Vec<String> = steps
        .iter()
        .map(|(request, response)| {
            format!(
                r#"{{"request": {}, "response": {response}}}"#,
                request.to_compact_string()
            )
        })
        .collect();
    format!(r#"{{"calls": [{}]}}"#, calls.join(","))
}

/// The recording with one more entry than the run uses.
fn with_extra_entry(text: &str) -> String {
    let body = text.strip_suffix("]}").expect("recording ends with ]}");
    format!("{body},{{\"response\": {{}}}}]}}")
}

fn seed(session: &mut dyn GremlinSession, confirmed: bool) -> (Result<(), SeedError>, Vec<String>) {
    let mut lines = Vec::new();
    let result = crate::docgraph::seed::run(session, "test.example", confirmed, &mut |l| {
        lines.push(l.to_string());
    });
    (result, lines)
}

#[test]
fn the_wipe_is_refused_before_any_call_without_confirmation() {
    struct Spy(usize);
    impl GremlinSession for Spy {
        fn submit(&mut self, _: &Json) -> Result<Vec<Json>, toolkit_aws::AwsError> {
            self.0 += 1;
            Ok(vec![])
        }
    }
    let mut spy = Spy(0);
    let (result, lines) = seed(&mut spy, false);
    assert!(matches!(result, Err(SeedError::WipeNotConfirmed)));
    assert_eq!(spy.0, 0, "no statement may be sent");
    assert_eq!(lines, ["Connecting to Neptune at test.example:8182..."]);
    assert!(
        WIPE_REFUSAL.contains("--confirm-wipe")
            && WIPE_REFUSAL.contains("NEPTUNE_SEED_CONFIRM_WIPE=1")
    );
}

#[test]
fn the_statements_are_the_scripts() {
    let phases = phases();
    assert_eq!(phases.len(), 3);
    assert_eq!(phases[0].statements.len(), 10);
    assert_eq!(phases[1].statements.len(), 12);
    assert_eq!(phases[2].statements.len(), 7);
    assert_eq!(phases[0].summary, "  Created 5 users, 5 follows edges");
    assert_eq!(
        phases[1].summary,
        "  Created 4 accounts, 3 identifiers, 5 USES edges"
    );
    assert_eq!(phases[2].summary, "  Created 3 products, 4 PURCHASED edges");
    assert_eq!(
        phases[0].statements[0],
        Statement {
            gremlin:
                "g.addV('Person').property('id', uid).property('name', name).property('age', age)"
                    .to_string(),
            bindings: Some(json(r#"{"uid": "U1", "name": "Alice", "age": 30}"#)),
        }
    );
    assert_eq!(
        phases[2].statements[0].bindings,
        Some(json(r#"{"pid": "P1", "name": "Widget", "price": 29.99}"#))
    );
    assert_eq!(
        phases[1].statements[11].gremlin,
        "g.V().has('Account','id',aid).addE('USES').to(g.V().has('Identifier','id',iid))"
    );
    // Every value travels as a binding, never spliced into the statement.
    for phase in &phases {
        for statement in &phase.statements {
            assert!(!statement.gremlin.contains("Alice") && !statement.gremlin.contains("\"U"));
        }
    }
}

#[test]
fn a_confirmed_run_replays_in_order_and_verifies() {
    let mut session = RecordedSession::from_document(&json(&recording(&[]))).expect("recording");
    let (result, lines) = seed(&mut session, true);
    result.expect("seeded");
    assert_eq!(session.unused(), 0);
    assert_eq!(lines[0], "Connecting to Neptune at test.example:8182...");
    assert_eq!(lines[1], "Clearing ALL graph data at test.example...");
    assert_eq!(lines[2], "Seeding social graph...");
    assert!(lines.contains(&"  Vertices: 12".to_string()));
    assert!(lines.contains(&"  Edges: 14".to_string()));
    assert!(
        lines.contains(&"  Accounts sharing identifiers with A2: ['A1', 'A2', 'A3']".to_string())
    );
    assert!(lines.contains(&"  Friends-of-friends for Alice: ['Dave']".to_string()));
    let tail: Vec<&str> = lines[lines.len() - 5..]
        .iter()
        .map(String::as_str)
        .collect();
    assert_eq!(
        tail,
        [
            "",
            "\u{2705} Seed data verified successfully",
            "",
            "Seed complete. Neptune test cluster is ready for eval runs.",
            "Endpoint: wss://test.example:8182/gremlin"
        ]
    );
}

#[test]
fn a_failed_verification_stops_the_run_with_the_script_message() {
    let one_account = recording(&[(
        "g.V().has('Account','id','A2').out('USES').in('USES').dedup().values('id').toList()",
        r#"{"results": ["A2"]}"#,
    )]);
    let mut session = RecordedSession::from_document(&json(&one_account)).expect("recording");
    let (result, _) = seed(&mut session, true);
    assert!(matches!(
        result,
        Err(SeedError::Verification(
            "Fraud ring query returned unexpected results"
        ))
    ));
    let nobody = recording(&[(
        "g.V().has('Person','id','U1').out('FOLLOWS').out('FOLLOWS').dedup().values('name').toList()",
        r#"{"results": []}"#,
    )]);
    let mut session = RecordedSession::from_document(&json(&nobody)).expect("recording");
    let (result, _) = seed(&mut session, true);
    assert!(matches!(
        result,
        Err(SeedError::Verification(
            "Friends-of-friends query returned unexpected results"
        ))
    ));
    let no_count = recording(&[("g.V().count()", r#"{"results": []}"#)]);
    let mut session = RecordedSession::from_document(&json(&no_count)).expect("recording");
    assert!(matches!(
        seed(&mut session, true).0,
        Err(SeedError::Call(_))
    ));
}

#[test]
fn a_changed_statement_or_a_short_recording_fails_and_extra_entries_are_reported() {
    let text = recording(&[]).replacen("Alice", "Mallory", 1);
    let mut session = RecordedSession::from_document(&json(&text)).expect("recording");
    assert!(matches!(
        seed(&mut session, true).0,
        Err(SeedError::Call(_))
    ));

    let doc = recording(&[]);
    let cut = doc.rfind(",{\"request\"").expect("last entry");
    let short = json(&format!("{}]}}", &doc[..cut]));
    let mut session = RecordedSession::from_document(&short).expect("recording");
    assert!(matches!(
        seed(&mut session, true).0,
        Err(SeedError::Call(_))
    ));

    let extra = with_extra_entry(&recording(&[]));
    let mut session = RecordedSession::from_document(&json(&extra)).expect("recording");
    seed(&mut session, true).0.expect("runs");
    assert_eq!(session.unused(), 1);
}

#[test]
fn recorded_seed_files_with_wrong_shapes_are_rejected_without_values() {
    for text in [
        r#"{}"#,
        r#"{"calls": {}}"#,
        r#"{"calls": [{}]}"#,
        r#"{"calls": [{"request": null, "response": {}}]}"#,
        r#"{"calls": [{"response": {"results": null}}]}"#,
        r#"{"calls": [{"response": {"results": "x"}}]}"#,
        r#"{"calls": [{"error": {"message": 3}}]}"#,
    ] {
        assert!(
            RecordedSession::from_document(&json(text)).is_err(),
            "{text}"
        );
    }
    let hostile = format!(r#"{{"calls": [{{"response": {{"results": "{SECRET}"}}}}]}}"#);
    let reason = RecordedSession::from_document(&json(&hostile))
        .err()
        .expect("rejected");
    assert!(!reason.contains(SECRET), "{reason}");
}

#[test]
fn a_recorded_failure_compares_its_request_first_and_hides_its_message() {
    let doc = format!(
        r#"{{"calls": [{{"request": {{"gremlin": "g.V().drop()"}}, "error": {{"message": "{SECRET}"}}}}]}}"#
    );
    let mut session = RecordedSession::from_document(&json(&doc)).expect("recording");
    let wrong = session.submit(&json(r#"{"gremlin": "other"}"#));
    assert!(wrong.is_err());
    let mut session = RecordedSession::from_document(&json(&doc)).expect("recording");
    let (result, _) = seed(&mut session, true);
    let Err(error) = result else {
        panic!("must fail")
    };
    let shown = crate::docgraph::seed_failure(&error).message().to_string();
    assert!(!shown.contains(SECRET), "{shown}");
}

#[test]
fn nothing_is_sent_without_a_recording() {
    let mut session = NoLiveSession;
    let (result, lines) = seed(&mut session, true);
    assert!(matches!(result, Err(SeedError::Call(_))));
    assert_eq!(lines.len(), 2);
}

#[test]
fn the_command_guards_the_wipe_and_never_leaks_an_endpoint() {
    let ran = run(&["neptune", "seed"]);
    assert_eq!(ran.status, 1);
    assert_eq!(ran.stdout, "Connecting to Neptune at localhost:8182...\n");
    assert!(
        ran.stderr.contains("Refusing to wipe the graph"),
        "{}",
        ran.stderr
    );
    assert!(ran.stderr.contains("--confirm-wipe"));

    let ran = run(&["neptune", "seed", "--confirm-wipe"]);
    assert_eq!(ran.status, 1);
    assert!(
        ran.stderr.contains("no live Gremlin client"),
        "{}",
        ran.stderr
    );

    let ran = run(&["neptune", "seed", "--endpoint", "a b"]);
    assert_eq!(ran.status, 2);
    let ran = run(&["neptune", "seed", "--endpoint", "evil.example/\u{1b}[31m"]);
    assert_eq!(ran.status, 2);
    assert!(!ran.stderr.contains('\u{1b}'));
    let ran = run(&["neptune", "seed", "--dry-run", "--recorded", "x"]);
    assert_eq!(ran.status, 2);
}

#[test]
fn the_command_replays_a_recording_and_rejects_unused_entries() {
    let dir = TempDir::new("seed");
    let file = dir.write("seed.json", &recording(&[]));
    let ran = run(&[
        "neptune",
        "seed",
        "--confirm-wipe",
        "--recorded",
        &file,
        "--endpoint",
        "n.example",
    ]);
    assert_eq!(ran.status, 0, "{}", ran.stderr);
    assert!(ran.stdout.starts_with("Connecting to Neptune at n.example:8182...\nClearing ALL graph data at n.example...\nSeeding social graph...\n"));
    assert!(ran
        .stdout
        .ends_with("Endpoint: wss://n.example:8182/gremlin\n"));

    let ran = run(&["neptune", "seed", "--recorded", &file]);
    assert_eq!(ran.status, 1);
    assert_eq!(ran.stdout, "Connecting to Neptune at localhost:8182...\n");

    let extra = dir.write("extra.json", &with_extra_entry(&recording(&[])));
    let ran = run(&["neptune", "seed", "--confirm-wipe", "--recorded", &extra]);
    assert_eq!(ran.status, 1);
    assert!(
        ran.stderr.contains("1 recorded call(s) were not used"),
        "{}",
        ran.stderr
    );
}

#[test]
fn the_dry_run_lists_every_statement_and_sends_nothing() {
    let lines = dry_run_lines();
    assert_eq!(lines[0], "Dry run: nothing is sent to Neptune.");
    assert_eq!(
        lines[1],
        "1. [destructive, needs --confirm-wipe] g.V().drop()"
    );
    assert_eq!(
        lines[2],
        "2. g.addV('Person').property('id', uid).property('name', name).property('age', age) {\"uid\":\"U1\",\"name\":\"Alice\",\"age\":30}"
    );
    assert_eq!(lines.len(), 2 + 29 + 4);
    let ran = run(&["neptune", "seed", "--dry-run"]);
    assert_eq!(ran.status, 0);
    assert_eq!(ran.stdout.lines().count(), lines.len());
}
