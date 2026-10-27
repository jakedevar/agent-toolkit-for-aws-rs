//! Static contract guards of the `llm-to-bedrock` skill (#4405).
//!
//! These are the documentation-text tests of `test_delta_reference_schema.py`
//! and `test_astra_execution_contract.py`, ported to read the plugin files
//! directly: a value the agent documents must be one the schemas accept, and
//! the documents that carry the Astra contract must keep saying so. The tests
//! of that Python file that execute Python code embedded in the documents, or
//! that import `image_input`, `bedrock_pricing`, `iam_policy` or
//! `preflight_bedrock`, are not ported here (see
//! `docs/migration/bedrock-report.md`, "Out of scope").

use std::fs;
use std::path::{Path, PathBuf};

use toolkit_core::{parse_json, Json};

use super::validate::load_schema;
use crate::startup::text::{is_space, is_word};

fn plugin_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/aws-startup-advisor")
}

fn read_bytes(relative: &str) -> Vec<u8> {
    let path = plugin_root().join(relative);
    fs::read(&path).unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

fn read(relative: &str) -> String {
    String::from_utf8(read_bytes(relative)).unwrap_or_else(|_| panic!("{relative} is not UTF-8"))
}

/// Python `text.split(marker, 1)[1]`.
fn after<'a>(text: &'a str, marker: &str) -> &'a str {
    text.split_once(marker)
        .unwrap_or_else(|| panic!("marker {marker:?} is missing"))
        .1
}

/// Python `text.split(marker, 1)[0]`.
fn before<'a>(text: &'a str, marker: &str) -> &'a str {
    text.split_once(marker).map_or(text, |(head, _)| head)
}

fn section<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    before(after(text, start), end)
}

fn all_contain(text: &str, needles: &[&str], origin: &str) {
    for needle in needles {
        assert!(text.contains(needle), "{origin} lost {needle:?}");
    }
}

// ---- resolution_kind and control-state reasons ------------------------------------------------

/// The `enum` next to the first member called `key` anywhere in `node`.
fn find_enum(node: &Json, key: &str) -> Option<Vec<String>> {
    match node {
        Json::Object(pairs) => {
            for (name, value) in pairs {
                if name == key {
                    if let Some(Json::Array(options)) = value.get("enum") {
                        return Some(
                            options
                                .iter()
                                .filter_map(Json::as_str)
                                .map(str::to_string)
                                .collect(),
                        );
                    }
                }
                if let Some(found) = find_enum(value, key) {
                    return Some(found);
                }
            }
            None
        }
        Json::Array(items) => items.iter().find_map(|item| find_enum(item, key)),
        _ => None,
    }
}

/// What follows `resolution_kind` in ``resolution_kind`?\s*:\s*`?([a-z_]+)`?``
/// and how many bytes the match consumed.
fn kind_after(rest: &str) -> Option<(String, usize)> {
    let total = rest.len();
    let rest = rest.strip_prefix('`').unwrap_or(rest);
    let rest = rest.trim_start_matches(is_space);
    let rest = rest.strip_prefix(':')?;
    let rest = rest.trim_start_matches(is_space);
    let rest = rest.strip_prefix('`').unwrap_or(rest);
    let kind: String = rest
        .chars()
        .take_while(|c| c.is_ascii_lowercase() || *c == '_')
        .collect();
    if kind.is_empty() {
        return None;
    }
    let rest = &rest[kind.len()..];
    let rest = rest.strip_prefix('`').unwrap_or(rest);
    Some((kind, total - rest.len()))
}

/// The values of every `resolution_kind` declaration or mention on `line`,
/// in the bullet form and in prose, with the value inside or outside the
/// backticks.
fn resolution_kinds(line: &str) -> Vec<String> {
    const KEY: &str = "resolution_kind";
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(offset) = line[from..].find(KEY) {
        let start = from + offset + KEY.len();
        if let Some((kind, used)) = kind_after(&line[start..]) {
            found.push(kind);
            from = start + used;
        } else {
            from += offset + 1;
        }
    }
    found
}

/// The values of every ``reason:\s*'([a-z_]+)'`` on `line`.
fn quoted_reasons(line: &str) -> Vec<String> {
    const KEY: &str = "reason:";
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(offset) = line[from..].find(KEY) {
        let start = from + offset + KEY.len();
        let rest = line[start..].trim_start_matches(is_space);
        let value = rest.strip_prefix('\'').and_then(|quoted| {
            let name: String = quoted
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || *c == '_')
                .collect();
            let closed = quoted[name.len()..].starts_with('\'');
            (!name.is_empty() && closed).then_some(name)
        });
        if let Some(name) = value {
            found.push(name);
        }
        from += offset + 1;
    }
    found
}

fn markdown_files(relative: &str) -> Vec<PathBuf> {
    let dir = plugin_root().join(relative);
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", dir.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
        .collect();
    files.sort();
    files
}

#[test]
fn the_resolution_kind_scan_finds_both_declaration_forms() {
    // Guards the guard: if the scan stops matching either form, the test
    // below silently passes on a broken reference.
    assert_eq!(
        resolution_kinds("- `resolution_kind`: `impl_path`"),
        ["impl_path"]
    );
    assert_eq!(
        resolution_kinds("always `user_visible: false`, `resolution_kind: mechanical`."),
        ["mechanical"]
    );
    assert_eq!(
        resolution_kinds("resolution_kind : ux_choice and resolution_kind: `impl_path`"),
        ["ux_choice", "impl_path"]
    );
    assert!(resolution_kinds("the resolution_kind field").is_empty());
    assert!(resolution_kinds("resolution_kind: 7").is_empty());
}

#[test]
fn every_reference_resolution_kind_is_accepted_by_the_analysis_schema() {
    let schema = load_schema("analysis").expect("embedded analysis schema");
    let allowed = find_enum(&schema, "resolution_kind")
        .expect("analysis.json no longer declares a resolution_kind enum");
    let references = "skills/llm-to-bedrock/references/helpers/behavior-delta-detection/references";
    let files = markdown_files(references);
    assert!(!files.is_empty(), "no delta references found");
    let mut offenders = Vec::new();
    let mut seen = 0;
    for file in files {
        let text = fs::read_to_string(&file).expect("read reference");
        let name = file
            .file_name()
            .expect("file name")
            .to_string_lossy()
            .into_owned();
        for (index, line) in text.lines().enumerate() {
            for kind in resolution_kinds(line) {
                seen += 1;
                if !allowed.contains(&kind) {
                    offenders.push(format!("{name}:{} -> {kind:?}", index + 1));
                }
            }
        }
    }
    assert!(seen > 0, "the references declare no resolution_kind at all");
    assert!(
        offenders.is_empty(),
        "delta references instruct the analyzer to emit resolution_kind values that \
         analysis.json rejects (allowed: {allowed:?}):\n  {}",
        offenders.join("\n  ")
    );
}

fn reason_enum(branch: &Json, key: &str) -> Vec<String> {
    branch
        .get("properties")
        .and_then(|properties| properties.get(key))
        .and_then(|state| state.get("properties"))
        .and_then(|properties| properties.get("reason"))
        .and_then(|reason| reason.get("enum"))
        .and_then(|options| match options {
            Json::Array(options) => Some(
                options
                    .iter()
                    .filter_map(Json::as_str)
                    .map(str::to_string)
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

#[test]
fn every_evaluator_control_state_reason_is_representable() {
    let schema = load_schema("eval").expect("embedded eval schema");
    let Some(Json::Array(branches)) = schema.get("oneOf") else {
        panic!("eval.json lost its oneOf");
    };
    let mut allowed = Vec::new();
    for branch in branches {
        allowed.extend(reason_enum(branch, "blocked"));
        allowed.extend(reason_enum(branch, "partial"));
    }
    assert!(allowed.contains(&"model_access".to_string()), "{allowed:?}");
    let prompt = read("agents/llm2bedrock-prompt-evaluator.md");
    let mut offenders = Vec::new();
    let mut seen = 0;
    for (index, line) in prompt.lines().enumerate() {
        for reason in quoted_reasons(line) {
            seen += 1;
            if !allowed.contains(&reason) {
                offenders.push(format!(
                    "llm2bedrock-prompt-evaluator.md:{} -> {reason:?}",
                    index + 1
                ));
            }
        }
    }
    assert!(seen > 0, "the evaluator documents no control-state reason");
    assert!(
        offenders.is_empty(),
        "the evaluator documents control-state reasons that eval.json rejects \
         ({allowed:?}):\n  {}",
        offenders.join("\n  ")
    );
}

// ---- evaluator prompt guards ----------------------------------------------------------------------

/// ``"role":\s*"(user|developer|system|assistant)"`` anywhere in `window`.
fn has_role_wrapper(window: &str) -> bool {
    window.match_indices("\"role\":").any(|(index, key)| {
        let rest = window[index + key.len()..].trim_start_matches(is_space);
        rest.strip_prefix('"').is_some_and(|quoted| {
            ["user", "developer", "system", "assistant"]
                .iter()
                .any(|role| {
                    quoted
                        .strip_prefix(role)
                        .is_some_and(|tail| tail.starts_with('"'))
                })
        })
    })
}

#[test]
fn the_evaluator_does_not_document_a_bare_responses_content_list() {
    let prompt = read("agents/llm2bedrock-prompt-evaluator.md");
    let lines: Vec<&str> = prompt.lines().collect();
    let mut real = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let lineno = index + 1;
        if !line.contains("input_image") && !line.contains("input_text") {
            continue;
        }
        // A content block inside a role wrapper, or one that delegates to the
        // helper, is fine; the wrapped §9.5a snippet puts the role up to three
        // lines above the block.
        if line.contains("\"role\"")
            || line.contains("'role'")
            || line.contains("responses_message")
        {
            continue;
        }
        let window = lines[lineno.saturating_sub(4)..lineno].join("\n");
        if !has_role_wrapper(&window) {
            real.push(format!("line {lineno}: {}", line.trim()));
        }
    }
    assert!(
        real.is_empty(),
        "Responses image content documented without a user-message wrapper \
         (input takes [{{'role':'user','content':[...]}}], not a bare block list):\n  {}",
        real.join("\n  ")
    );
}

#[test]
fn the_role_wrapper_scan_matches_the_four_message_roles() {
    assert!(has_role_wrapper(r#"{"role": "user", "content": []}"#));
    assert!(has_role_wrapper("x\n\"role\":\"developer\""));
    assert!(has_role_wrapper(r#""role":   "assistant""#));
    assert!(!has_role_wrapper(r#"{"role": "tool"}"#));
    assert!(!has_role_wrapper(r#"{"role": "users"}"#));
    assert!(!has_role_wrapper("input_image"));
}

#[test]
fn the_evaluator_never_templates_an_extension_into_a_mime_type() {
    // `image/<ext>` yields the invalid `image/jpg` for a .jpg case.
    let prompt = read("agents/llm2bedrock-prompt-evaluator.md");
    let bad: Vec<String> = prompt
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            line.contains("image/<ext>")
                || line.contains("'format': <ext>")
                || line.contains("\"format\": <ext>")
        })
        .map(|(index, line)| format!("line {}: {}", index + 1, line.trim()))
        .collect();
    assert!(
        bad.is_empty(),
        "extension templated directly into a wire format; use image_input helpers \
         (.jpg -> jpeg / image/jpeg):\n  {}",
        bad.join("\n  ")
    );
}

#[test]
fn documented_responses_image_blocks_set_detail() {
    let prompt = read("agents/llm2bedrock-prompt-evaluator.md");
    let bad: Vec<String> = prompt
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            line.contains("\"type\": \"input_image\"") && !line.contains("\"detail\"")
        })
        .map(|(index, line)| format!("line {}: {}", index + 1, line.trim()))
        .collect();
    assert!(
        bad.is_empty(),
        "inline input_image block without the SDK-required `detail` field:\n  {}",
        bad.join("\n  ")
    );
}

/// Field names of the canonical `prompts.jsonl` record in the log-ingestor prompt.
fn canonical_case_fields() -> Vec<String> {
    let text = read("agents/llm2bedrock-log-ingestor.md");
    let marker = "Write each entry as one JSON object per line";
    let block = before(after(after(&text, marker), "```json"), "```");
    let record = parse_json(block).expect("the canonical record is JSON");
    let Json::Object(pairs) = record else {
        panic!("the canonical record is not an object");
    };
    let fields: Vec<String> = pairs.into_iter().map(|(name, _)| name).collect();
    assert!(
        fields.contains(&"user_prompt".to_string()),
        "canonical record shape changed — revisit this guard"
    );
    fields
}

/// The keys read as ``case["key"]``, ``prompt['key']``, ``entry.get("key"`` on `line`.
fn accessed_keys(line: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for name in ["case", "prompt", "entry"] {
        for (index, _) in line.match_indices(name) {
            if line[..index].chars().next_back().is_some_and(is_word) {
                continue;
            }
            let rest = &line[index + name.len()..];
            let subscript = rest.strip_prefix('[');
            let Some(tail) = subscript.or_else(|| rest.strip_prefix(".get(")) else {
                continue;
            };
            let Some(tail) = tail.strip_prefix(['"', '\'']) else {
                continue;
            };
            let key: String = tail
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || *c == '_')
                .collect();
            if key.is_empty() {
                continue;
            }
            let closing = tail[key.len()..].strip_prefix(['"', '\'']);
            let closed = match (subscript.is_some(), closing) {
                (true, Some(after_quote)) => after_quote.starts_with(']'),
                (false, Some(_)) => true,
                (_, None) => false,
            };
            if closed {
                keys.push(key);
            }
        }
    }
    keys
}

#[test]
fn the_accessor_scan_reads_subscripts_and_getters() {
    assert_eq!(accessed_keys(r#"x = case["user_prompt"]"#), ["user_prompt"]);
    assert_eq!(
        accessed_keys("entry.get('image_path', None)"),
        ["image_path"]
    );
    assert_eq!(accessed_keys(r#"prompt["a"] + prompt['b']"#), ["a", "b"]);
    assert!(accessed_keys(r#"showcase["x"] prompts["y"] case["Z"]"#).is_empty());
    assert!(accessed_keys("case[index]").is_empty());
}

#[test]
fn the_evaluator_reads_only_canonical_golden_case_fields() {
    let fields = canonical_case_fields();
    let prompt = read("agents/llm2bedrock-prompt-evaluator.md");
    let mut offenders = Vec::new();
    let mut seen = 0;
    for (index, line) in prompt.lines().enumerate() {
        for key in accessed_keys(line) {
            seen += 1;
            if !fields.contains(&key) {
                offenders.push(format!(
                    "llm2bedrock-prompt-evaluator.md:{} -> {key:?}",
                    index + 1
                ));
            }
        }
    }
    assert!(seen > 0, "the evaluator reads no golden-case field at all");
    assert!(
        offenders.is_empty(),
        "evaluator reads golden-case fields absent from the canonical record \
         ({fields:?}):\n  {}",
        offenders.join("\n  ")
    );
}

// ---- Astra handoff contract (documentation text) ------------------------------------------------------

#[test]
fn dispatch_and_rewriter_preserve_the_runtime_openai_strategy() {
    let skill = read("skills/llm-to-bedrock/SKILL.md");
    let rewriter = read("agents/llm2bedrock-code-rewriter.md");
    all_contain(
        &skill,
        &[
            "validated_target_model_ids, plan_runtime_api",
            "set `rewrite_strategy = \"runtime_openai\"`",
            "Target API surface:",
        ],
        "SKILL.md",
    );
    let lane = section(
        &rewriter,
        "### Runtime OpenAI lane",
        "### Mantle express lane",
    );
    all_contain(
        lane,
        &[
            "bedrock-runtime.{region}.amazonaws.com/openai/v1",
            "chat.completions.create",
            "responses.create",
            "OpenAI-to-boto3 dependency replacement",
        ],
        "the runtime OpenAI lane",
    );
}

#[test]
fn api_handoff_paths_are_named_by_every_consumer() {
    let helper = read(
        "skills/llm-to-bedrock/references/helpers/behavior-delta-detection/behavior-delta-detection.md",
    );
    let reference = read(
        "skills/llm-to-bedrock/references/helpers/behavior-delta-detection/references/openai-to-bedrock.md",
    );
    assert!(helper.contains("Same-vendor GPT endpoint and API deltas"));
    assert!(reference.contains("## Same-vendor GPT endpoint and API deltas"));
    for relative in [
        "skills/llm-to-bedrock/SKILL.md",
        "skills/gcp-to-aws/references/phases/design/design-ai.md",
        "skills/gcp-to-aws/references/phases/generate/generate-ai.md",
        "skills/gcp-to-aws/references/vendored/ai/ai-openai-to-bedrock.md",
    ] {
        assert!(read(relative).contains("mantle_openai_chat"), "{relative}");
    }
}

#[test]
fn c3_and_c5_use_the_shared_normalization_instructions() {
    let skill = read("skills/llm-to-bedrock/SKILL.md");
    let before_dispatch = before(&skill, "### C0");
    let gate = section(&skill, "**Gate (a.5)", "**Gate (b)");
    assert!(before_dispatch.contains("normalize_api_path"));
    assert!(before_dispatch.contains("Target API path: <resolved_api_path"));
    assert!(gate.contains("Reuse") && gate.contains("resolved_api_path"));
    assert!(!gate.contains("field is\nabsent), set `rewrite_strategy = \"converse\"`"));
}

#[test]
fn the_astra_shared_contract_survives_each_vendored_consumer() {
    for name in [
        "ai-openai-to-bedrock.md",
        "ai-model-lifecycle.md",
        "ai-migration-guardrails.md",
        "bedrock-quotas.md",
    ] {
        let canonical = read_bytes(&format!("skills/shared/ai/{name}"));
        assert!(
            canonical.windows(5).any(|window| window == b"Astra"),
            "{name} no longer mentions Astra"
        );
        for skill in ["gcp-to-aws", "azure-to-aws"] {
            let vendored = read_bytes(&format!("skills/{skill}/references/vendored/ai/{name}"));
            assert!(vendored == canonical, "{skill} copy of {name} differs");
        }
    }
}

#[test]
fn the_astra_policy_has_consumer_local_facts_and_prices() {
    for skill in ["gcp-to-aws", "azure-to-aws"] {
        let facts = read(&format!(
            "skills/{skill}/references/shared/openai-on-bedrock.md"
        ));
        let prices = read(&format!(
            "skills/{skill}/references/vendored/ai/bedrock-pricing-cache.md"
        ));
        all_contain(
            &facts,
            &["openai.gpt-6-astra", "us-west-2", "30-minute cache-write"],
            skill,
        );
        all_contain(&prices, &["GPT-6 Astra", "Global CRIS", "82.50"], skill);
    }
}

#[test]
fn the_shared_astra_paths_reach_the_azure_schema_and_generator() {
    let refs = "skills/azure-to-aws/references";
    for relative in [
        "phases/design/design-ai.md",
        "shared/schema-design-aws-ai.md",
        "phases/generate/generate-artifacts-ai.md",
    ] {
        let text = read(&format!("{refs}/{relative}"));
        assert!(text.contains("mantle_openai_chat"), "{relative}");
        assert!(text.contains("runtime_openai_cris"), "{relative}");
    }
    let generator = read(&format!("{refs}/phases/generate/generate-artifacts-ai.md"));
    let row = generator
        .lines()
        .find(|line| line.starts_with("| `mantle_openai_responses`"))
        .expect("the generator table has a mantle_openai_responses row");
    assert!(
        row.contains("mantle_openai_chat") && row.contains("migrate_to_mantle.sh"),
        "{row}"
    );
}

#[test]
fn the_azure_runtime_parent_gate_matches_the_catalog_witnesses() {
    // A source-contract test, not execution of the natural-language DSL.
    let catalog = parse_json(&read(
        "skills/agent-advisor/references/models/openai-bedrock-2026-09-24.json",
    ))
    .expect("the catalog is JSON");
    let regions = catalog
        .get("models")
        .and_then(|models| models.get("openai_gpt_6_astra"))
        .and_then(|model| model.get("paths"))
        .and_then(|paths| paths.get("runtime_converse"))
        .and_then(|path| path.get("inference_profile_regions"))
        .expect("the catalog lists Astra runtime profile regions");
    for (model_id, region, allowed) in [
        ("us.openai.gpt-6-astra", "us-east-1", true),
        ("global.openai.gpt-6-astra", "eu-west-1", true),
        ("openai.gpt-6-astra", "us-east-1", false),
    ] {
        let listed = match regions.get(model_id) {
            Some(Json::Array(items)) => items.iter().any(|item| item.as_str() == Some(region)),
            _ => false,
        };
        assert_eq!(listed, allowed, "{model_id} in {region}");
    }
    let refs = "skills/azure-to-aws/references";
    for parent in ["phases/design/design.md", "phases/generate/generate.md"] {
        let text = read(&format!("{refs}/{parent}"));
        let frontmatter = before(after(&text, "---"), "---");
        let rule = frontmatter
            .lines()
            .find(|line| line.contains("runtime_openai_cris accepts documented"))
            .unwrap_or_else(|| panic!("{parent} lost the runtime_openai_cris rule"));
        all_contain(
            rule,
            &[
                "bare proprietary openai.gpt-* IDs use Mantle only",
                "Astra us./global.",
                "caller-region checks",
            ],
            parent,
        );
        assert!(
            !frontmatter.contains("no proprietary openai.gpt-* model ID is paired"),
            "{parent}"
        );
    }
    let schema = read(&format!("{refs}/shared/schema-design-aws-ai.md"));
    assert!(schema.contains("**`code_migration`** — `migration_path`"));
}

#[test]
fn the_roi_and_recommendation_use_the_computed_comparison() {
    for skill in ["gcp-to-aws", "azure-to-aws"] {
        let text = read(&format!(
            "skills/{skill}/references/phases/estimate/estimate-ai.md"
        ));
        let roi = section(&text, "## Part 5: ROI Analysis", "## Part 6:");
        assert!(
            roi.contains("mark the source comparison unavailable"),
            "{skill}"
        );
        assert!(
            !roi.contains("projected cost is **about 10% higher**"),
            "{skill}"
        );
        let recommendation = section(&text, "## Part 7: Migration Recommendation", "## Output");
        all_contain(
            recommendation,
            &[
                "computed Part 5 result, not a fixed premium",
                "source comparison is unavailable",
            ],
            skill,
        );
        assert!(!recommendation.contains("~10% higher"), "{skill}");
        assert!(!recommendation.contains("costs ~10% more"), "{skill}");
    }
}

#[test]
fn gcp_stage_two_artifacts_follow_the_mantle_and_runtime_contracts() {
    let text = read("skills/gcp-to-aws/references/phases/generate/generate-artifacts-ai.md");
    let route = section(&text, "## Step 0:", "## Step 1M:");
    for api_path in [
        "mantle_openai_chat",
        "mantle_openai_responses",
        "runtime_openai_cris",
    ] {
        assert!(route.contains(api_path), "{api_path}");
    }
    assert!(route.contains("starts with `\"mantle\"`"));
    let setup = section(
        &text,
        "## Step 3: Generate Bedrock Setup Script",
        "## Step 3B",
    );
    all_contain(
        setup,
        &[
            "bedrock-mantle:CreateInference",
            "bedrock-mantle:CallWithBearerToken",
            "test_comparison.py --target-only --quick",
            "A Mantle selection must never run a Converse probe",
            "also authorize that exact `application-inference-profile/",
            "effective invocation target and IAM remain aligned",
        ],
        "the setup script step",
    );
    let monitoring = after(&text, "## Step 3F:");
    all_contain(
        monitoring,
        &[
            "omit the profile resource and its output for Mantle targets",
            "copy_from = \"{verified_model_source_arn}\"",
            "Astra supports application inference profiles only on runtime Converse",
        ],
        "the monitoring step",
    );
    assert!(!monitoring.contains("prefixed with \"us.\" for US regions"));
}

#[test]
fn the_gcp_report_lists_only_generated_profile_attribution() {
    let text = read("skills/gcp-to-aws/references/phases/generate/generate-artifacts-report.md");
    let controls = section(&text, "**Cost guardrails", "**What the baseline");
    let rows: Vec<&str> = controls
        .lines()
        .filter(|line| line.starts_with('|'))
        .collect();
    assert!(rows.iter().any(|row| row.contains("Bedrock budget")));
    assert!(rows
        .iter()
        .any(|row| row.contains("Cost anomaly detection")));
    assert!(!rows.iter().any(|row| row.contains("Inference profiles")));
    all_contain(
        controls,
        &[
            "actual generated `bedrock_monitoring.tf`",
            "enabled `aws_bedrock_inference_profile` resource",
            "`bedrock_inference_profile_arns` output",
        ],
        "the cost guardrails",
    );
    assert!(text.contains("add profile attribution only when an eligible profile was generated"));
}

#[test]
fn the_gcp_design_producer_uses_the_current_cost_and_quota_contract() {
    let text = read("skills/gcp-to-aws/references/phases/design/design-ai.md");
    let assessment = section(
        &text,
        "**Stay-or-migrate assessment per model:**",
        "**Model comparison table**",
    );
    assert!(assessment.contains("verified source") && assessment.contains("unavailable"));
    assert!(!assessment.contains("report a modest cost increase rather than parity"));
    let quota = section(&text, "**Quota risk assessment**", "## Part 1B:");
    let rows: Vec<Vec<&str>> = quota
        .lines()
        .filter(|line| line.starts_with('|'))
        .map(|line| {
            line.trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().trim_matches(|c| c == '`' || c == '"'))
                .collect()
        })
        .collect();
    let astra = rows
        .iter()
        .find(|row| {
            row.get(1)
                .is_some_and(|cell| cell.contains("Astra runtime"))
        })
        .expect("the quota table has an Astra runtime row");
    assert!(
        astra.first() == Some(&"medium") && astra.get(2) == Some(&"medium"),
        "{astra:?}"
    );
    assert!(astra[1].contains("10"), "{astra:?}");
    assert!(!rows.iter().any(|row| row
        .get(1)
        .is_some_and(|cell| cell.contains("other (1× burndown)"))));
}
