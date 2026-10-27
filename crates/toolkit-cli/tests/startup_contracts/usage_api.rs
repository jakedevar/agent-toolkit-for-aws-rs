//! `test_usage_api_request_params.py`: usage-API request parameters match
//! each provider's documented limits.

use crate::common::{check, ensure, name, Plugin, Verdict};
use crate::edit_replace;
use crate::rules::{check_rules, Rule, Scope};

const OPENAI: &str = "skills/azure-to-aws/references/phases/discover/discover-openai-api.md";
const ANTHROPIC: [&str; 2] = [
    "skills/gcp-to-aws/references/phases/discover/discover-anthropic-api.md",
    "skills/azure-to-aws/references/phases/discover/discover-anthropic-api.md",
];

/// Every `limit=N` that follows `prefix_pattern` on one line.
fn limits(prefix_pattern: &str, text: &str) -> Vec<String> {
    let re = fancy_regex::Regex::new(&format!("{prefix_pattern}.*?limit=(\\d+)")).expect("pattern");
    re.captures_iter(text)
        .map(|c| c.expect("capture")[1].to_owned())
        .collect()
}

fn usage_rows_are_capped(p: &Plugin) -> Verdict {
    let found = limits(r"/v1/organization/usage/\S+", &p.read(OPENAI)?);
    ensure!(
        !found.is_empty(),
        "expected to find usage-endpoint rows with a limit= parameter"
    );
    for limit in found {
        ensure!(
            limit != "180",
            "a usage-endpoint row still requests limit=180 (OpenAI's usage endpoints cap bucket_width=1d&limit at 31)"
        );
        ensure!(
            limit == "31",
            "usage-endpoint row requests limit={limit}, expected limit=31"
        );
    }
    Ok(())
}

#[test]
fn test_azure_openai_usage_rows_do_not_request_limit_180() {
    check(
        usage_rows_are_capped,
        &[
            (
                edit_replace!(
                    OPENAI,
                    "`limit=31`      | `usage-completions.json`",
                    "`limit=180`     | `usage-completions.json`"
                ),
                "limit=180",
            ),
            (
                edit_replace!(
                    OPENAI,
                    "`limit=31`      | `usage-images.json`",
                    "`limit=50`      | `usage-images.json`"
                ),
                "expected limit=31",
            ),
            (
                edit_replace!(OPENAI, "/v1/organization/usage/", "/v1/organization/use/"),
                "expected to find",
            ),
        ],
    );
}

fn costs_row_keeps_180(p: &Plugin) -> Verdict {
    let found = limits(r"/v1/organization/costs", &p.read(OPENAI)?);
    ensure!(
        !found.is_empty(),
        "expected to find a costs-endpoint row with a limit= parameter"
    );
    ensure!(
        found.iter().all(|l| l == "180"),
        "the Costs endpoint row's limit=180 should be unchanged"
    );
    Ok(())
}

#[test]
fn test_azure_openai_costs_row_keeps_limit_180() {
    check(
        costs_row_keeps_180,
        &[
            (
                edit_replace!(
                    OPENAI,
                    "`project_ids[]…`, `limit=180` | `costs.json`",
                    "`project_ids[]…`, `limit=31` | `costs.json`"
                ),
                "should be unchanged",
            ),
            (
                edit_replace!(OPENAI, "/v1/organization/costs", "/v1/organization/bills"),
                "expected to find",
            ),
        ],
    );
}

#[test]
fn test_azure_openai_has_copy_paste_trap_callout() {
    check_rules(&[Rule::new(OPENAI).all(&["copy-paste trap"])]);
}

fn step_of<'a>(text: &'a str, after: &str, before: &str) -> &'a str {
    // text.split(after, 1)[-1].split(before, 1)[0]
    let tail = text.split_once(after).map_or(text, |(_, t)| t);
    tail.split_once(before).map_or(tail, |(h, _)| h)
}

fn cost_report_has_workspace_id(p: &Plugin) -> Verdict {
    for f in ANTHROPIC {
        let text = p.read(f)?;
        ensure!(
            step_of(&text, "## Step 3", "## Step 4").contains("workspace_id"),
            "{}: Step 3's costs_by_description[] schema no longer carries workspace_id per row",
            name(f)
        );
        ensure!(
            text.contains("\"workspace_id\": \"wrkspc_abc\""),
            "{}: the costs_by_description[] example row is missing workspace_id",
            name(f)
        );
    }
    Ok(())
}

#[test]
fn test_anthropic_cost_report_carries_workspace_id_per_row() {
    check(
        cost_report_has_workspace_id,
        &[(
            edit_replace!(
                ANTHROPIC[0],
                "\"workspace_id\": \"wrkspc_abc\"",
                "\"workspace\": \"wrkspc_abc\""
            ),
            "example row",
        )],
    );
}

fn single_call_compound_group_by(p: &Plugin) -> Verdict {
    for f in ANTHROPIC {
        let text = p.read(f)?;
        let step2 = step_of(&text, "## Step 2:", "## Step 3");
        let normalized = step2
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        ensure!(
            normalized.contains("group_by[]=workspace_id&group_by[]=description"),
            "{}: Step 2c no longer documents a single cost_report call grouped by both workspace_id and description",
            name(f)
        );
        let row =
            fancy_regex::Regex::new(r"(?m)\|\s*2\s*\|\s*`/v1/organizations/cost_report`.*?\|\s*$")
                .expect("pattern")
                .find(step2)
                .expect("search")
                .ok_or_else(|| format!("{}: could not find the cost_report table row", name(f)))?;
        ensure!(
            !row.as_str().contains("workspace_ids[]="),
            "{}: the cost_report table row still requests workspace_ids[]=",
            name(f)
        );
    }
    Ok(())
}

#[test]
fn test_anthropic_cost_report_uses_single_call_with_compound_group_by() {
    check(
        single_call_compound_group_by,
        &[
            (
                edit_replace!(
                    ANTHROPIC[1],
                    "group_by[]=workspace_id&group_by[]=description",
                    "group_by[]=description"
                ),
                "compound",
            ),
            (
                edit_replace!(
                    ANTHROPIC[0],
                    "(one call total, no `workspace_ids[]` param)",
                    "(one call total, `workspace_ids[]=wrkspc_abc`)"
                ),
                "still requests workspace_ids[]=",
            ),
            (
                edit_replace!(
                    ANTHROPIC[0],
                    "`/v1/organizations/cost_report` |",
                    "`/v1/organizations/cost` |"
                ),
                "could not find",
            ),
        ],
    );
}

#[test]
fn test_anthropic_probe_uses_bracket_group_by() {
    let step2 = Scope::Span("## Step 2:", "## Step 3");
    check_rules(&[
        Rule::new(ANTHROPIC[0])
            .scope(step2)
            .all(&["group_by[]=workspace_id&limit=31"])
            .none(&["group_by=workspace_id"]),
        Rule::new(ANTHROPIC[1])
            .scope(step2)
            .all(&["group_by[]=workspace_id&limit=31"])
            .none(&["group_by=workspace_id"]),
    ]);
}

#[test]
fn test_anthropic_usage_messages_row_uses_bracket_group_by() {
    let step2 = Scope::Span("## Step 2:", "## Step 3");
    check_rules(&[
        Rule::new(ANTHROPIC[0])
            .scope(step2)
            .all(&["group_by[]=model&group_by[]=service_tier"])
            .none(&["group_by=model,service_tier"]),
        Rule::new(ANTHROPIC[1])
            .scope(step2)
            .all(&["group_by[]=model&group_by[]=service_tier"])
            .none(&["group_by=model,service_tier"]),
    ]);
}

#[test]
fn test_anthropic_consent_block_discloses_preselection_probe() {
    let step0 = Scope::Span("## Step 0:", "## Step 1:");
    check_rules(&[
        Rule::new(ANTHROPIC[0])
            .scope(step0)
            .all(&["Never captured", "Step 2b"]),
        Rule::new(ANTHROPIC[1])
            .scope(step0)
            .all(&["Never captured", "Step 2b"]),
    ]);
}
