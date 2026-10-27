//! `test_gpt6_source_breakpoint.py`, `test_report_generator_cost_pricing.py`
//! and `test_report_generator_capture_warnings_gate.py`.
//!
//! The first two Python files extracted the one embedded ```python block of
//! `agents/llm2bedrock-report-generator.md` (the `cost_compare.py` an agent
//! writes and runs) and executed it. Running Python is outside this
//! repository's rules and the block is shipped agent content, so the Rust
//! port has two halves that stay tied together:
//!
//! * [`PINNED`] lists the exact statements of the embedded script that carry
//!   the pricing decisions (breakpoint comparison, cost formulas, basis
//!   labels, ultrafast rule, output formats). A test requires every one to be
//!   present in the committed block, so an edit of the script's logic fails
//!   here and sends the reader to the model below.
//! * [`price`] is an executable model of exactly those statements and the
//!   numeric cases of the Python tests run against it, reproducing the
//!   Python tests' expected output lines.
//!
//! The model cannot detect a change in the script that leaves every pinned
//! statement in place; that limit is recorded in
//! `docs/migration/startup-contracts.md`, together with the follow-up that
//! would remove it (a `toolkit` command for the cost comparison).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::common::{check, ensure, Plugin, Verdict};
use crate::edit_replace;

const REPORT_GENERATOR: &str = "agents/llm2bedrock-report-generator.md";

/// The sole embedded python block, as `PYTHON_BLOCK_RE.findall` returns it.
fn embedded_script(p: &Plugin) -> Result<String, String> {
    let text = p.read(REPORT_GENERATOR)?;
    let mut blocks = Vec::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("```python\n") {
        let body = &rest[start + "```python\n".len()..];
        let Some(end) = body.find("\n```") else { break };
        blocks.push(&body[..end]);
        rest = &body[end + 4..];
    }
    ensure!(
        blocks.len() == 1,
        "expected exactly one embedded python block in llm2bedrock-report-generator.md, found {}",
        blocks.len()
    );
    Ok(blocks[0].to_owned())
}

/// Statements of the embedded script the model below reproduces.
const PINNED: [&str; 24] = [
    "if b_in_tok > 272_000 and s_in_long is not None:",
    "bucket_source_cost = (b_in_tok * s_in_long + b_out_tok * s_out_long) / 1_000_000",
    "bucket_source_cost = (b_in_tok * s_in + b_out_tok * s_out) / 1_000_000",
    "if bucket_source_is_long and tgt in BEDROCK_LONG_CONTEXT:",
    "source_cost += count * bucket_source_cost",
    "bedrock_cost += count * bucket_bedrock_cost",
    "basis = \"tier-distribution\"",
    "basis = \"baseline\"",
    "basis = \"sample\"",
    "source_is_long = bool(per_request and per_request[\"input\"] > 272_000 and s_in_long is not None)",
    "source_cost = (in_tok * s_in_long + out_tok * s_out_long) / 1_000_000",
    "source_cost = (in_tok * s_in + out_tok * s_out) / 1_000_000",
    "if source_is_long and tgt in BEDROCK_LONG_CONTEXT:",
    "bedrock_cost = (in_tok * long_rates[\"input\"] + out_tok * long_rates[\"output\"]) / 1_000_000",
    "bedrock_cost = (in_tok * b_in + out_tok * b_out) / 1_000_000",
    "basis = f\"{basis}-long-context\"",
    "basis = f\"{basis}-source-long-context\"",
    "if SERVICE_TIER_BY_SOURCE.get(src) == \"ultrafast\":",
    "if any(name in src for name in ULTRAFAST_SUPPORTED):",
    "source_cost *= 6",
    "bedrock_cost *= 6",
    "basis = f\"{basis}-ultrafast-unavailable\"",
    "print(f\"TOTAL source=${total_source_cost:.4f} bedrock=${total_bedrock_cost:.4f}\")",
    "print(f\"Estimated savings: {savings_pct:+.1f}%\")",
];

fn pinned_statements_present(p: &Plugin) -> Verdict {
    let script = embedded_script(p)?;
    let lines: Vec<&str> = script.lines().map(str::trim).collect();
    for statement in PINNED {
        ensure!(
            lines.contains(&statement),
            "the embedded cost_compare script no longer contains the statement {statement:?}; \
             update the executable model in cost_compare.rs with it"
        );
    }
    Ok(())
}

#[test]
fn the_embedded_cost_script_keeps_the_statements_the_model_reproduces() {
    check(
        pinned_statements_present,
        &[
            (
                edit_replace!(
                    REPORT_GENERATOR,
                    "per_request[\"input\"] > 272_000",
                    "per_request[\"input\"] >= 272_000"
                ),
                "source_is_long",
            ),
            (
                edit_replace!(REPORT_GENERATOR, "source_cost *= 6", "source_cost *= 5"),
                "source_cost *= 6",
            ),
        ],
    );
}

fn exactly_one_python_block(p: &Plugin) -> Verdict {
    embedded_script(p).map(|_| ())
}

#[test]
fn the_report_generator_embeds_exactly_one_python_block() {
    check(
        exactly_one_python_block,
        &[(
            edit_replace!(
                REPORT_GENERATOR,
                "## 6.3",
                "```python\nprint(1)\n```\n\n## 6.3"
            ),
            "found 2",
        )],
    );
}

// ---- the model ------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Pair {
    src: &'static str,
    tgt: &'static str,
    s_in: f64,
    s_out: f64,
    b_in: f64,
    b_out: f64,
    /// Optional long-context source rates (the 7th and 8th tuple elements).
    long: Option<(f64, f64)>,
}

#[derive(Default)]
struct Inputs {
    pairs: Vec<Pair>,
    /// `BASELINE_TOKENS_BY_SOURCE`: model -> (input, output).
    baseline: BTreeMap<&'static str, (f64, f64)>,
    /// `PER_REQUEST_TOKENS`.
    per_request: BTreeMap<&'static str, (f64, f64)>,
    /// `TIER_DISTRIBUTION_BY_SOURCE`: model -> [(count, input, output)].
    tiers: BTreeMap<&'static str, Vec<(f64, f64, f64)>>,
    /// `BEDROCK_LONG_CONTEXT`: target -> (input, output) rates.
    bedrock_long: BTreeMap<&'static str, (f64, f64)>,
    /// `SERVICE_TIER_BY_SOURCE` entries equal to "ultrafast".
    ultrafast: Vec<&'static str>,
    /// Golden-dataset sample rows: (source_model, prompt, completion).
    sample: Vec<(&'static str, f64, f64)>,
}

/// Python's `f"{x:.4f}"` for the amounts the cases use.
fn money(x: f64) -> String {
    format!("{x:.4}")
}

/// Python's `f"{x:+.1f}"`.
fn signed_pct(x: f64) -> String {
    format!("{x:+.1}")
}

fn price(inputs: &Inputs) -> String {
    let mut sample: BTreeMap<&str, (f64, f64)> = BTreeMap::new();
    for (src, prompt, completion) in &inputs.sample {
        let e = sample.entry(src).or_default();
        e.0 += prompt;
        e.1 += completion;
    }
    let mut total_source = 0.0;
    let mut total_bedrock = 0.0;
    let mut out = String::new();
    for pair in &inputs.pairs {
        let (src, tgt) = (pair.src, pair.tgt);
        let mut basis: String;
        let (mut in_tok, mut out_tok) = (0.0, 0.0);
        let (mut source_cost, mut bedrock_cost);
        if let Some(buckets) = inputs.tiers.get(src) {
            basis = "tier-distribution".into();
            in_tok = buckets.iter().map(|b| b.0 * b.1).sum();
            out_tok = buckets.iter().map(|b| b.0 * b.2).sum();
            source_cost = 0.0;
            bedrock_cost = 0.0;
            for &(count, bi, bo) in buckets {
                let (cost_s, long) = match pair.long {
                    Some((li, lo)) if bi > 272_000.0 => ((bi * li + bo * lo) / 1_000_000.0, true),
                    _ => ((bi * pair.s_in + bo * pair.s_out) / 1_000_000.0, false),
                };
                let cost_b = match inputs.bedrock_long.get(tgt) {
                    Some(&(ri, ro)) if long => (bi * ri + bo * ro) / 1_000_000.0,
                    _ => (bi * pair.b_in + bo * pair.b_out) / 1_000_000.0,
                };
                source_cost += count * cost_s;
                bedrock_cost += count * cost_b;
            }
        } else {
            let bucket = if let Some(b) = inputs.baseline.get(src) {
                basis = "baseline".into();
                *b
            } else {
                basis = "sample".into();
                sample.get(src).copied().unwrap_or((0.0, 0.0))
            };
            (in_tok, out_tok) = bucket;
            let source_is_long =
                inputs.per_request.get(src).is_some_and(|r| r.0 > 272_000.0) && pair.long.is_some();
            source_cost = match pair.long {
                Some((li, lo)) if source_is_long => (in_tok * li + out_tok * lo) / 1_000_000.0,
                _ => (in_tok * pair.s_in + out_tok * pair.s_out) / 1_000_000.0,
            };
            bedrock_cost = match inputs.bedrock_long.get(tgt) {
                Some(&(ri, ro)) if source_is_long => (in_tok * ri + out_tok * ro) / 1_000_000.0,
                _ => (in_tok * pair.b_in + out_tok * pair.b_out) / 1_000_000.0,
            };
            if source_is_long && inputs.bedrock_long.contains_key(tgt) {
                basis = format!("{basis}-long-context");
            } else if source_is_long {
                basis = format!("{basis}-source-long-context");
            }
        }
        let mut unavailable = false;
        if inputs.ultrafast.contains(&src) {
            if ["gpt-6-astra", "gpt-6.1-sol"]
                .iter()
                .any(|n| src.contains(n))
            {
                source_cost *= 6.0;
                bedrock_cost *= 6.0;
                basis = format!("{basis}-ultrafast");
            } else {
                unavailable = true;
                basis = format!("{basis}-ultrafast-unavailable");
            }
        }
        if unavailable {
            let _ = writeln!(
                out,
                "{src} -> {tgt} ({basis}): input={in_tok} output={out_tok} ultrafast not supported for this model — cost unavailable"
            );
            continue;
        }
        total_source += source_cost;
        total_bedrock += bedrock_cost;
        let _ = writeln!(
            out,
            "{src} -> {tgt} ({basis}): input={in_tok} output={out_tok} source=${} bedrock=${}",
            money(source_cost),
            money(bedrock_cost)
        );
    }
    let _ = writeln!(
        out,
        "TOTAL source=${} bedrock=${}",
        money(total_source),
        money(total_bedrock)
    );
    if total_source > 0.0 {
        let pct = (total_source - total_bedrock) / total_source * 100.0;
        let _ = writeln!(out, "Estimated savings: {}%", signed_pct(pct));
    }
    out
}

fn sol(long: Option<(f64, f64)>) -> Pair {
    Pair {
        src: "openai.gpt-6-sol",
        tgt: "openai.gpt-6-sol",
        s_in: 2.0,
        s_out: 10.0,
        b_in: 2.2,
        b_out: 11.0,
        long,
    }
}

const SOL_LONG: (f64, f64) = (4.0, 15.0);
const SOL: &str = "openai.gpt-6-sol";

fn sol_inputs(input: f64, per_request_input: f64) -> Inputs {
    let mut i = Inputs {
        pairs: vec![sol(Some(SOL_LONG))],
        ..Inputs::default()
    };
    i.baseline.insert(SOL, (input, 1_000_000.0));
    i.per_request.insert(SOL, (per_request_input, 10_000.0));
    i.bedrock_long.insert(SOL, (4.4, 16.5));
    i
}

// ---- test_gpt6_source_breakpoint.py ----------------------------------------------

#[test]
fn test_gpt6_sol_300k_input_crosses_breakpoint_on_both_sides() {
    let stdout = price(&sol_inputs(30_000_000.0, 300_000.0));
    assert!(stdout.contains("baseline-long-context"), "{stdout}");
    assert!(stdout.contains("source=$135.0000"), "{stdout}");
    assert!(stdout.contains("bedrock=$148.5000"), "{stdout}");
    assert!(!stdout.contains("source=$70.0000"), "{stdout}");
    assert!(stdout.contains("Estimated savings: -10.0%"), "{stdout}");
    assert!(
        !stdout.contains("-112.1%") && !stdout.contains("-112.14%"),
        "{stdout}"
    );
}

#[test]
fn test_pair_without_per_request_tokens_keeps_flat_rate_unchanged() {
    let mut i = sol_inputs(30_000_000.0, 300_000.0);
    i.per_request.clear();
    let stdout = price(&i);
    assert!(stdout.contains("(baseline):"), "{stdout}");
    assert!(!stdout.contains("long-context"), "{stdout}");
    assert!(
        stdout.contains(&format!(
            "source=${}",
            money((30_000_000.0 * 2.0 + 1_000_000.0 * 10.0) / 1e6)
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "bedrock=${}",
            money((30_000_000.0 * 2.2 + 1_000_000.0 * 11.0) / 1e6)
        )),
        "{stdout}"
    );
}

#[test]
fn test_exact_boundary_272000_input_tokens_stays_short_context() {
    let stdout = price(&sol_inputs(27_200_000.0, 272_000.0));
    assert!(!stdout.contains("long-context"), "{stdout}");
    assert!(
        stdout.contains(&format!(
            "source=${}",
            money((27_200_000.0 * 2.0 + 1_000_000.0 * 10.0) / 1e6)
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "bedrock=${}",
            money((27_200_000.0 * 2.2 + 1_000_000.0 * 11.0) / 1e6)
        )),
        "{stdout}"
    );
}

#[test]
fn test_exact_boundary_272001_input_tokens_reprices_whole_request() {
    let stdout = price(&sol_inputs(27_200_100.0, 272_001.0));
    assert!(stdout.contains("long-context"), "{stdout}");
    assert!(
        stdout.contains(&format!(
            "source=${}",
            money((27_200_100.0 * 4.0 + 1_000_000.0 * 15.0) / 1e6)
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "bedrock=${}",
            money((27_200_100.0 * 4.4 + 1_000_000.0 * 16.5) / 1e6)
        )),
        "{stdout}"
    );
}

fn tier_inputs() -> Inputs {
    let mut i = Inputs {
        pairs: vec![sol(Some(SOL_LONG))],
        ..Inputs::default()
    };
    i.tiers.insert(
        SOL,
        vec![(51.0, 300_000.0, 10_000.0), (49.0, 200_000.0, 10_000.0)],
    );
    i.bedrock_long.insert(SOL, (4.4, 16.5));
    i
}

#[test]
fn test_mixed_tier_distribution_partitions_before_pricing() {
    let stdout = price(&tier_inputs());
    assert!(stdout.contains("tier-distribution"), "{stdout}");
    assert!(stdout.contains("source=$93.3500"), "{stdout}");
    assert!(stdout.contains("bedrock=$102.6850"), "{stdout}");
    assert!(!stdout.contains("source=$115.4000"), "{stdout}");
    assert!(!stdout.contains("bedrock=$126.9400"), "{stdout}");
}

#[test]
fn test_tier_distribution_absent_falls_back_to_existing_behavior() {
    let i = sol_inputs(30_000_000.0, 300_000.0);
    assert!(i.tiers.is_empty());
    let stdout = price(&i);
    assert!(stdout.contains("source=$135.0000"), "{stdout}");
    assert!(stdout.contains("bedrock=$148.5000"), "{stdout}");
}

#[test]
fn test_source_breakpoint_independent_of_flat_target() {
    let (b_in, b_out) = (1.675, 2.0);
    assert_eq!(
        format!("{:.4}", 100.0 * (300_000.0 * b_in + 10_000.0 * b_out) / 1e6),
        "52.2500"
    );
    let mut i = Inputs {
        pairs: vec![Pair {
            src: SOL,
            tgt: "flat-target",
            s_in: 2.0,
            s_out: 10.0,
            b_in,
            b_out,
            long: Some(SOL_LONG),
        }],
        ..Inputs::default()
    };
    i.baseline.insert(SOL, (30_000_000.0, 1_000_000.0));
    i.per_request.insert(SOL, (300_000.0, 10_000.0));
    let stdout = price(&i);
    assert!(stdout.contains("source=$135.0000"), "{stdout}");
    assert!(stdout.contains("bedrock=$52.2500"), "{stdout}");
    assert!(stdout.contains("Estimated savings: +61.3%"), "{stdout}");
    assert!(!stdout.contains("source=$70.0000"), "{stdout}");
    assert!(!stdout.contains("+25.4%"), "{stdout}");
    assert!(!stdout.contains("-112.1%"), "{stdout}");
}

// ---- test_report_generator_cost_pricing.py ---------------------------------------

fn gpt4o(sample: Vec<(&'static str, f64, f64)>, baseline: bool) -> Inputs {
    let mut i = Inputs {
        pairs: vec![Pair {
            src: "gpt-4o",
            tgt: "anthropic.claude-sonnet-4-6",
            s_in: 2.5,
            s_out: 10.0,
            b_in: 3.0,
            b_out: 15.0,
            long: None,
        }],
        sample,
        ..Inputs::default()
    };
    if baseline {
        i.baseline.insert("gpt-4o", (42_000_000.0, 9_000_000.0));
    }
    i
}

#[test]
fn test_baseline_covered_pair_prices_both_sides_from_baseline_volume() {
    let stdout = price(&gpt4o(
        vec![("gpt-4o", 1000.0, 400.0), ("gpt-4o", 1000.0, 400.0)],
        true,
    ));
    assert!(
        stdout.contains(
            "gpt-4o -> anthropic.claude-sonnet-4-6 (baseline): input=42000000 output=9000000"
        ),
        "{stdout}"
    );
    assert!(!stdout.contains("input=2000"), "{stdout}");
    let source = (42_000_000.0 * 2.5 + 9_000_000.0 * 10.0) / 1e6;
    let bedrock = (42_000_000.0 * 3.0 + 9_000_000.0 * 15.0) / 1e6;
    assert!(
        stdout.contains(&format!("source=${}", money(source))),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("bedrock=${}", money(bedrock))),
        "{stdout}"
    );
    let pct = (source - bedrock) / source * 100.0;
    assert!(
        stdout.contains(&format!("Estimated savings: {}%", signed_pct(pct))),
        "{stdout}"
    );
}

#[test]
fn test_pair_not_covered_by_baseline_falls_back_to_golden_dataset_sample() {
    let mut i = Inputs {
        pairs: vec![Pair {
            src: "gpt-4o-mini",
            tgt: "amazon.nova-lite-v1:0",
            s_in: 0.15,
            s_out: 0.6,
            b_in: 0.06,
            b_out: 0.24,
            long: None,
        }],
        sample: vec![("gpt-4o-mini", 500.0, 200.0)],
        ..Inputs::default()
    };
    i.baseline.insert("gpt-4o", (42_000_000.0, 9_000_000.0));
    let stdout = price(&i);
    assert!(
        stdout.contains("gpt-4o-mini -> amazon.nova-lite-v1:0 (sample): input=500 output=200"),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "source=${}",
            money((500.0 * 0.15 + 200.0 * 0.6) / 1e6)
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "bedrock=${}",
            money((500.0 * 0.06 + 200.0 * 0.24) / 1e6)
        )),
        "{stdout}"
    );
}

#[test]
fn test_no_baseline_at_all_matches_pre_fix_golden_dataset_behavior() {
    let stdout = price(&gpt4o(
        vec![("gpt-4o", 1000.0, 400.0), ("gpt-4o", 1000.0, 400.0)],
        false,
    ));
    assert!(
        stdout.contains("gpt-4o -> anthropic.claude-sonnet-4-6 (sample): input=2000 output=800"),
        "{stdout}"
    );
}

fn gpt61(model: &'static str) -> Inputs {
    let mut i = Inputs {
        pairs: vec![Pair {
            src: model,
            tgt: model,
            s_in: 2.0,
            s_out: 10.0,
            b_in: 2.2,
            b_out: 11.0,
            long: None,
        }],
        ..Inputs::default()
    };
    i.baseline.insert(model, (1_000_000.0, 100_000.0));
    i.ultrafast.push(model);
    i
}

#[test]
fn test_gpt6_1_sol_ultrafast_applies_6x_multiplier() {
    let stdout = price(&gpt61("us.openai.gpt-6.1-sol"));
    assert!(stdout.contains("-ultrafast"), "{stdout}");
    assert!(stdout.contains("source=$18.0000"), "{stdout}");
    assert!(stdout.contains("bedrock=$19.8000"), "{stdout}");
    assert!(
        stdout.contains("TOTAL source=$18.0000 bedrock=$19.8000"),
        "{stdout}"
    );
}

#[test]
fn test_gpt6_sol_ultrafast_request_reports_cost_unavailable() {
    let stdout = price(&gpt61("us.openai.gpt-6-sol"));
    assert!(
        stdout.contains("ultrafast not supported for this model — cost unavailable"),
        "{stdout}"
    );
    let row = stdout
        .lines()
        .find(|l| l.starts_with("us.openai.gpt-6-sol ->"))
        .expect("row");
    assert!(
        !row.contains("source=$"),
        "Sol must not be silently priced at Standard for an ultrafast request"
    );
    assert!(
        stdout.contains("TOTAL source=$0.0000 bedrock=$0.0000"),
        "{stdout}"
    );
}

#[test]
fn test_pair_with_no_service_tier_entry_prices_as_standard_unchanged() {
    let model = "us.openai.gpt-6.1-sol";
    let i = Inputs {
        pairs: vec![Pair {
            src: model,
            tgt: model,
            s_in: 2.0,
            s_out: 10.0,
            b_in: 2.2,
            b_out: 11.0,
            long: None,
        }],
        sample: vec![(model, 1000.0, 400.0)],
        ..Inputs::default()
    };
    let stdout = price(&i);
    assert!(!stdout.contains("-ultrafast"), "{stdout}");
    assert!(
        stdout.contains(&format!(
            "source=${}",
            money((1000.0 * 2.0 + 400.0 * 10.0) / 1e6)
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "bedrock=${}",
            money((1000.0 * 2.2 + 400.0 * 11.0) / 1e6)
        )),
        "{stdout}"
    );
}

// ---- test_report_generator_capture_warnings_gate.py ------------------------------

struct Baseline {
    blended: bool,
    cost_statuses: &'static [&'static str],
    warnings: usize,
}

/// The §6.3 gate rule as the Python test modelled it.
fn should_print_unqualified_savings_pct(b: &Baseline) -> bool {
    !b.blended && !b.cost_statuses.contains(&"cost_unavailable") && b.warnings == 0
}

#[test]
fn test_section_6_3_reads_capture_warnings_independently() {
    use crate::rules::{check_rules, Rule, Scope};
    check_rules(&[Rule::new(REPORT_GENERATOR)
        .scope(Scope::Span(
            "## 6.3 Sum token usage",
            "The migration plan may map multiple source models",
        ))
        .all(&[
            "`capture_warnings`",
            "independently of the two checks above",
            "do NOT print an unqualified savings/premium percentage",
        ])]);
}

#[test]
fn test_full_window_available_cost_baseline_with_mixed_exclusion_is_qualified() {
    let baseline = Baseline {
        blended: false,
        cost_statuses: &["complete"],
        warnings: 1,
    };
    let old_gate_allows =
        !baseline.blended && !baseline.cost_statuses.contains(&"cost_unavailable");
    assert!(
        old_gate_allows,
        "sanity: old two-gate logic missed this case"
    );
    assert!(!should_print_unqualified_savings_pct(&baseline));
}

#[test]
fn test_empty_capture_warnings_still_allows_unqualified_percentage() {
    let baseline = Baseline {
        blended: false,
        cost_statuses: &["complete"],
        warnings: 0,
    };
    assert!(should_print_unqualified_savings_pct(&baseline));
}
