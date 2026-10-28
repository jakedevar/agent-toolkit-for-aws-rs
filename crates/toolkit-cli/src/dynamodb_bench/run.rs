//! `main()` of `benchmark_model.py`: budget, guardrail, the invocation loop and
//! the summary.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use toolkit_aws::recorded::{optional_member, optional_object, optional_text};
use toolkit_aws::{redact_text, AwsError};
use toolkit_core::{parse_json, Json, ToolkitError};

use super::recorded::LambdaClient;
use super::response::{self, LambdaResponse};
use crate::dynamodb::bench::{
    apply_mode_preset, attach_vector_index_meta, cfg_f64, compute_split, estimate_bench_spend,
    guardrail_refusal, items, minutes, spend_line, validate, wall_clock,
};
use crate::dynamodb::bench_fmt::{
    diag, diag_bounded, diag_list, die, dumps_indent, fixed, isoformat_utc,
};
use crate::dynamodb::bench_rows::{aggregate, BenchRow, ExactCounts};
use crate::dynamodb::pyfmt;
use crate::safe_text::safe_path;
use crate::Output;

/// What `benchmark-model` was asked to do.
pub(super) struct Args<'a> {
    pub(super) model: &'a Path,
    pub(super) config: &'a Path,
    pub(super) manifest: &'a Path,
    pub(super) raw_out: &'a Path,
    pub(super) summary_out: &'a Path,
    pub(super) allow_spend: bool,
    pub(super) now: Option<&'a str>,
}

/// The wall clock of the run, or the instant `--now` pins.
pub(super) struct Clock {
    pinned: Option<String>,
}

fn is_timestamp(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 20
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            10 => *byte == b'T',
            13 | 16 => *byte == b':',
            19 => *byte == b'Z',
            _ => byte.is_ascii_digit(),
        })
}

impl Clock {
    pub(super) fn new(now: Option<&str>) -> Result<Self, ToolkitError> {
        let pinned = match now {
            None => None,
            Some(given) if is_timestamp(given) => {
                Some(format!("{}+00:00", given.trim_end_matches('Z')))
            }
            Some(_) => {
                return Err(die(
                    "--now must look like 2026-01-31T12:00:00Z (UTC, whole seconds)",
                ))
            }
        };
        Ok(Self { pinned })
    }

    /// `datetime.now(timezone.utc).isoformat()`.
    fn iso_now(&self) -> String {
        if let Some(pinned) = &self.pinned {
            return pinned.clone();
        }
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        isoformat_utc(
            i64::try_from(elapsed.as_secs()).unwrap_or(0),
            elapsed.subsec_micros(),
        )
    }

    /// Seconds since `earlier` (zero when pinned).
    fn since(&self, earlier: Instant) -> f64 {
        if self.pinned.is_some() {
            0.0
        } else {
            earlier.elapsed().as_secs_f64()
        }
    }
}

fn invoke_failure(error: &AwsError) -> ToolkitError {
    match error {
        AwsError::NoCredentials => die(
            "AWS credentials not found for the benchmark profile. Try: aws sso login --profile \
             <profile>",
        ),
        AwsError::Service(service) => die(format!(
            "lambda invoke failed ({}): {}",
            diag(service.code().unwrap_or("Unknown")),
            diag(service.message())
        )),
        AwsError::Connect { .. } | AwsError::Timeout { .. } => die(format!(
            "cannot reach AWS endpoint: {}",
            diag(&error.to_string())
        )),
        other => die(format!(
            "lambda invoke failed: {}: {}",
            other.code(),
            diag(&other.to_string())
        )),
    }
}

/// `_invoke_lambda`: the parsed payload of one invocation.
fn invoke_lambda(
    client: &mut dyn LambdaClient,
    function_name: &str,
    payload: &Json,
) -> Result<Json, ToolkitError> {
    let request = Json::object(vec![
        ("FunctionName", Json::string(function_name)),
        ("InvocationType", Json::string("RequestResponse")),
        ("Payload", payload.clone()),
    ]);
    let resp = client.invoke(&request).map_err(|e| invoke_failure(&e))?;
    let shape = |reason: String| die(format!("the Invoke response is invalid: {reason}"));
    let status = optional_member(&resp, "StatusCode", "an integer", |value| match value {
        Json::Int(number) => Some(*number),
        _ => None,
    })
    .map_err(|e| shape(e.to_string()))?
    .unwrap_or(0);
    let as_object = optional_object(&resp, "Payload");
    let as_text = optional_text(&resp, "Payload");
    let parsed = match (as_object, as_text) {
        (Ok(Some(body)), _) => body.clone(),
        (_, Ok(Some(body))) => parse_json(body).map_err(|_| {
            die(format!(
                "Lambda returned non-JSON payload (status {status}): {}",
                diag_bounded(body, 200)
            ))
        })?,
        (Ok(None), Ok(None)) => return Err(shape("\"Payload\" is required".to_string())),
        _ => {
            return Err(shape(
                "\"Payload\" must be an object or a string".to_string(),
            ))
        }
    };
    let function_error = optional_text(&resp, "FunctionError").map_err(|e| shape(e.to_string()))?;
    if let Some(code) = function_error.filter(|code| !code.is_empty()) {
        return Err(die(format!(
            "Lambda function error ({}): {}",
            diag(code),
            diag_bounded(&pyfmt::dumps(&parsed, false), 500)
        )));
    }
    Ok(parsed)
}

fn phase_list(phases: &[&str]) -> String {
    let parts: Vec<String> = phases
        .iter()
        .map(|phase| match *phase {
            "settle" | "seed" | "warmup" | "measure" => pyfmt::repr_str(phase),
            other => diag(other),
        })
        .collect();
    format!("[{}]", parts.join(", "))
}

fn manifest_text(manifest: &Json, key: &str) -> Json {
    manifest.get(key).cloned().unwrap_or(Json::Null)
}

/// `main()` of `benchmark_model.py`.
#[allow(clippy::too_many_lines)]
pub(super) fn run(
    args: &Args<'_>,
    client: &mut dyn LambdaClient,
    clock: &Clock,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let model = super::load(args.model, "the design JSON")?;
    let cfg_raw = super::load(args.config, "the benchmark config")?;
    let cfg = apply_mode_preset(&cfg_raw, out)?;
    let manifest = super::load(args.manifest, "the manifest")?;

    validate(&model)?;
    let patterns = items(&model, "access_patterns", "the design")?;
    if !matches!(manifest, Json::Object(_)) {
        return Err(die("the manifest must be a JSON object"));
    }
    let Some(lambda_block) = manifest.get("lambda") else {
        return Err(die(
            "manifest has no 'lambda' block — this benchmark_model.py expects a Lambda-based run. \
             Re-run deploy_model.py on the current version of the skill.",
        ));
    };
    for key in ["aws_profile", "region"] {
        if !matches!(cfg.get(key), Some(Json::String(text)) if !text.is_empty()) {
            return Err(die(format!(
                "the benchmark config needs a non-empty string `{key}`"
            )));
        }
    }
    let function_name = match lambda_block.get("function_name") {
        Some(Json::String(name))
            if !name.is_empty()
                && name.len() <= 170
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':')) =>
        {
            name.as_str()
        }
        _ => {
            return Err(die(
                "the manifest `lambda.function_name` must be a function name or ARN \
                 (letters, digits, '-', '_' and ':', at most 170 characters)",
            ))
        }
    };

    let n_patterns = patterns.len();
    let (invocations_total, _) = compute_split(&cfg, n_patterns)?;
    if invocations_total < 0 {
        return Err(die(
            "Even a single Lambda invocation's settle+seed+warmup overhead exceeds the \
             configured lambda_timeout_seconds. Either increase lambda_timeout_seconds (max 900), \
             reduce warmup_seconds, or reduce the number of access patterns benchmarked per run.",
        ));
    }
    out.line(&format!(
        "Benchmark budget: {n_patterns} pattern(s), splitting measurement across \
         {invocations_total} Lambda invocation(s)."
    ))?;

    let wall = wall_clock(&cfg, n_patterns, invocations_total)?;
    let est_min = minutes(wall.total);
    out.line(&format!(
        "Estimated wall-clock: ~{est_min} min ({}s: {}s settle + ~{}s seed + {n_patterns} \
         patterns x ({}s warmup + {}s measure), serial).",
        wall.total, wall.settle, wall.seed, wall.warmup, wall.duration
    ))?;
    if wall.total > 110 {
        out.line(&format!(
            "  ┌─ RUN THIS IN THE FOREGROUND AND WAIT ─────────────────────────┐\n  │ This run takes \
             ~{est_min} min, longer than a default tool/shell │\n  │ timeout. Do NOT background \
             it: a backgrounded run can be       │\n  │ killed mid-flight, leaving a STALE \
             perf_summary.json that      │\n  │ looks fresh. Raise your tool's timeout to exceed \
             the estimate  │\n  │ above and let this command block to completion. Verify         \
             │\n  │ benchmark_completed_at in the summary post-dates launch.       │\n  \
             └────────────────────────────────────────────────────────────────┘"
        ))?;
    }

    let guardrail = cfg_f64(&cfg, "cost_guardrail_usd", 0.50)?;
    let spend = estimate_bench_spend(&model, &cfg)?;
    out.line(&spend_line(&spend, guardrail))?;
    if spend.total_usd > guardrail && !args.allow_spend {
        return Err(die(guardrail_refusal(&spend, guardrail)).with_exit_code(3));
    }

    let tables = match model.get("tables") {
        Some(value) if pyfmt::truthy(value) => value.clone(),
        _ => Json::Array(Vec::new()),
    };

    let mut all_rows: Vec<BenchRow> = Vec::new();
    let mut tainted_overall: Vec<(String, String)> = Vec::new();
    let mut coverage_union: BTreeSet<String> = BTreeSet::new();
    let mut seed_verification = Json::Object(Vec::new());
    let mut first_invocation_errored = false;
    let mut echo_mode: Option<String> = None;
    let mut echo_key_sampling: Option<String> = None;
    let mut echo_items_per_partition: Option<i64> = None;
    let mut exact_counts = ExactCounts::new();
    let mut completed_all = true;

    let run_started_at = clock.iso_now();
    let wall_start = Instant::now();

    for idx in 0..invocations_total {
        let is_first = idx == 0;
        let phase_plan: &[&str] = if is_first {
            &["settle", "seed", "warmup", "measure"]
        } else {
            &["measure"]
        };
        let payload = Json::object(vec![
            (
                "phase_plan",
                Json::Array(phase_plan.iter().map(|p| Json::string(*p)).collect()),
            ),
            ("invocation_index", Json::Int(idx)),
            ("invocations_total", Json::Int(invocations_total)),
            ("patterns", Json::Array(attach_vector_index_meta(&model)?)),
            ("tables", tables.clone()),
            ("manifest", manifest.clone()),
            ("config", cfg.clone()),
        ]);
        out.line(&format!(
            "Invoking Lambda (invocation {}/{invocations_total}, phases={}) …",
            idx + 1,
            phase_list(phase_plan)
        ))?;
        let started = Instant::now();
        let parsed = invoke_lambda(client, function_name, &payload)?;
        let resp: LambdaResponse = response::parse(&parsed)
            .map_err(|reason| die(format!("the Lambda response is invalid: {reason}")))?;
        let dt = clock.since(started);
        out.line(&format!(
            "  returned in {}s, {} rows, phases_run={}",
            fixed(dt, 1),
            resp.raw_rows.len(),
            resp.phases_run.as_ref().map_or_else(
                || "None".to_string(),
                |phases| phase_list(&phases.iter().map(String::as_str).collect::<Vec<_>>())
            )
        ))?;

        if is_first {
            seed_verification = resp.seed_verification.clone();
            if resp.seed_verification_failed {
                out.diagnostic("  ! seed verification failed — aborting further invocations.");
                all_rows.extend(resp.raw_rows);
                first_invocation_errored = true;
                completed_all = false;
                break;
            }
            if let Some(message) = &resp.handler_error {
                out.diagnostic(&format!(
                    "  ! Lambda handler error: {}",
                    diag_bounded(&redact_text(message), 500)
                ));
                all_rows.extend(resp.raw_rows);
                first_invocation_errored = true;
                completed_all = false;
                break;
            }
            echo_mode = resp.mode.clone();
            echo_key_sampling = resp.key_sampling.clone();
            echo_items_per_partition = resp.items_per_partition;
        }

        all_rows.extend(resp.raw_rows);
        for (pid, reason) in resp.measurement_tainted {
            if let Some(slot) = tainted_overall.iter_mut().find(|(name, _)| *name == pid) {
                slot.1 = reason;
            } else {
                tainted_overall.push((pid, reason));
            }
        }
        coverage_union.extend(resp.measured_patterns);
        for ec in resp.exact_counts {
            let agg = exact_counts.entry((ec.pattern_id, ec.phase)).or_default();
            agg.calls += ec.agg.calls;
            agg.throttles += ec.agg.throttles;
            agg.errors += ec.agg.errors;
            for (code, count) in &ec.agg.error_codes.0 {
                agg.error_codes.add(code, *count);
            }
            for (code, count) in &ec.agg.cancellation_reason_codes.0 {
                agg.cancellation_reason_codes.add(code, *count);
            }
        }
    }
    if completed_all && client.unconsumed() > 0 {
        return Err(die(format!(
            "the recorded document has {} step(s) this run never used",
            client.unconsumed()
        )));
    }

    // Raw rows.
    let mut raw = String::new();
    for row in &all_rows {
        raw.push_str(&pyfmt::dumps(&row.json, false));
        raw.push('\n');
    }
    super::write(args.raw_out, &raw)?;
    out.line(&format!(
        "Raw rows: {} ({} rows)",
        safe_path(&args.raw_out.display().to_string()),
        all_rows.len()
    ))?;

    // Aggregate.
    let aggregates = aggregate(
        &all_rows,
        patterns,
        items(&model, "tables", "the design")?,
        &cfg,
        &exact_counts,
    )?;
    let per_pattern: Vec<Json> = aggregates
        .iter()
        .map(|item| {
            let tainted = item.id_text.as_deref().and_then(|id| {
                tainted_overall
                    .iter()
                    .find(|(name, _)| name == id)
                    .map(|(_, reason)| reason.as_str())
            });
            item.to_json(tainted)
        })
        .collect();

    let mut declared: BTreeSet<String> = BTreeSet::new();
    for item in &aggregates {
        let Some(id) = &item.id_text else {
            return Err(die("every access pattern needs a string `pattern_id`"));
        };
        declared.insert(id.clone());
    }
    let missing: Vec<String> = declared.difference(&coverage_union).cloned().collect();
    let coverage_incomplete = !missing.is_empty() || first_invocation_errored;

    let wall_seconds = if clock.pinned.is_some() {
        0.0
    } else {
        pyfmt::round(wall_start.elapsed().as_secs_f64(), 1)
    };
    let completed_at = clock.iso_now();
    let echo_or = |echoed: Option<Json>, key: &str, default: Json| -> Json {
        echoed.unwrap_or_else(|| cfg.get(key).cloned().unwrap_or(default))
    };
    let summary = Json::object(vec![
        ("run_id", manifest_text(&manifest, "run_id")),
        ("account", manifest_text(&manifest, "account")),
        ("region", manifest_text(&manifest, "region")),
        ("prefix", manifest_text(&manifest, "prefix")),
        (
            "manifest",
            Json::object(vec![
                ("account", manifest_text(&manifest, "account")),
                ("region", manifest_text(&manifest, "region")),
                ("prefix", manifest_text(&manifest, "prefix")),
                ("run_id", manifest_text(&manifest, "run_id")),
            ]),
        ),
        ("config", cfg.clone()),
        ("run_started_at", Json::string(run_started_at)),
        ("benchmark_completed_at", Json::string(completed_at.clone())),
        ("benchmark_wall_seconds", Json::Float(wall_seconds)),
        (
            "total_rows",
            Json::Int(i64::try_from(all_rows.len()).unwrap_or(i64::MAX)),
        ),
        (
            "mode",
            echo_or(
                echo_mode.map(Json::String),
                "mode",
                Json::string("standard"),
            ),
        ),
        (
            "key_sampling",
            echo_or(
                echo_key_sampling.map(Json::String),
                "read_pattern_key_sampling",
                Json::string("uniform"),
            ),
        ),
        (
            "items_per_partition",
            echo_or(
                echo_items_per_partition.map(Json::Int),
                "items_per_partition",
                Json::Int(1),
            ),
        ),
        ("invocations_total", Json::Int(invocations_total)),
        ("seed_verification", seed_verification),
        (
            "coverage",
            Json::object(vec![
                (
                    "measured_patterns",
                    Json::Array(
                        coverage_union
                            .iter()
                            .map(|p| Json::string(p.clone()))
                            .collect(),
                    ),
                ),
                (
                    "missing_patterns",
                    Json::Array(missing.iter().map(|p| Json::string(p.clone())).collect()),
                ),
                ("coverage_incomplete", Json::Bool(coverage_incomplete)),
            ]),
        ),
        ("patterns", Json::Array(per_pattern)),
    ]);
    super::write(args.summary_out, &dumps_indent(&summary))?;
    out.line(&format!(
        "Summary: {}",
        safe_path(&args.summary_out.display().to_string())
    ))?;
    out.line(&format!(
        "Benchmark completed at {completed_at} ({}s wall, {} rows) — verify this timestamp is \
         newer than when you launched the run before trusting the report.",
        pyfmt::float_repr(wall_seconds),
        all_rows.len()
    ))?;

    if coverage_incomplete {
        out.diagnostic(&format!(
            "\nWARNING: coverage incomplete — missing: {}",
            diag_list(&missing)
        ));
        if first_invocation_errored {
            out.set_exit_status(1);
        }
    }
    Ok(())
}
