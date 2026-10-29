//! DocumentDB and Neptune analysis and data utilities (#4395, slice
//! `docgraph`).
//!
//! | Command | Replaces |
//! | --- | --- |
//! | `toolkit documentdb wa-review` | `amazon-documentdb/scripts/wa_review.py` |
//! | `toolkit neptune validate-input` | `amazon-neptune/scripts/input_validator.py` |
//! | `toolkit neptune seed` | `amazon-neptune/scripts/seed_test_data.py` |
//!
//! `cdk_test_env.py` (a CDK stack for a throwaway Neptune cluster) is replaced
//! by the declarative CloudFormation template
//! `skills/specialized-skills/database-skills/amazon-neptune/scripts/neptune_test_env.cfn.json`,
//! whose security properties are asserted by this module's tests. No command
//! creates an environment.
//!
//! No command opens a socket, a database connection or reads a credential in
//! this build. Every AWS or database interaction goes through a seam
//! ([`client::AwsClient`], [`seed::GremlinSession`]) with a recorded
//! implementation (`--recorded FILE`) and a "no live client" implementation.
//! Live AWS and Gremlin bindings and the helper libraries
//! `graphrag_pipeline.py` and `agentic_memory.py` are follow-ups. The `--uri`
//! database collection of `wa_review.py` is replaced by a user-run shell recipe and
//! the offline `capture` normalizer (`toolkit documentdb normalize-capture`). Parity
//! evidence and the intentional changes are in `docs/migration/docgraph.md`.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::missing_errors_doc,
    clippy::too_many_lines
)]

use std::path::Path;

use toolkit_aws::{is_valid_region, Env, ProcessEnv};
use toolkit_core::{parse_json, Json, ToolkitError};

use crate::ai_data::parse_failure_text;
use crate::aurora::args::usage;
use crate::cloudwatch::timefmt::parse_iso_timestamp;
use crate::dynamodb::iterate::utc_iso;
use crate::startup::read_text;
use crate::{CommandSpec, Invocation, Output};

mod access;
mod capture;
mod checks;
mod client;
mod dbchecks;
mod infra;
pub mod memory;
mod neptune_input;
mod report;
mod seed;
mod snapshot;
mod tables;

#[cfg(test)]
mod tests;

use crate::safe_text::{safe_path, safe_text};
use client::{AwsClient, NoLiveClient, RecordedClient};
use seed::{GremlinSession, NoLiveSession, RecordedSession, SeedError};

/// `documentdb wa-review`.
pub const WA_REVIEW: CommandSpec = CommandSpec {
    path: &["documentdb", "wa-review"],
    flags: &["--live"],
    value_flags: &[
        "--cluster-id",
        "--region",
        "--analysis-data",
        "--recorded",
        "--output",
        "--now",
        "--network-policy",
    ],
    strict_values: true,
    repeatable_flags: &[],
    max_positionals: 0,
    passthrough: false,
    handler: wa_review_command,
};

/// `documentdb normalize-capture`.
pub const NORMALIZE_CAPTURE: CommandSpec = CommandSpec {
    path: &["documentdb", "normalize-capture"],
    flags: &[],
    value_flags: &["--capture", "--output"],
    strict_values: true,
    repeatable_flags: &[],
    max_positionals: 0,
    passthrough: false,
    handler: normalize_capture_command,
};

/// `neptune validate-input`.
pub const NEPTUNE_VALIDATE_INPUT: CommandSpec = CommandSpec {
    path: &["neptune", "validate-input"],
    flags: &[],
    value_flags: &[],
    strict_values: false,
    repeatable_flags: &[],
    max_positionals: usize::MAX,
    passthrough: false,
    handler: validate_input_command,
};

/// `neptune seed`.
pub const NEPTUNE_SEED: CommandSpec = CommandSpec {
    path: &["neptune", "seed"],
    flags: &["--confirm-wipe", "--dry-run", "--live"],
    value_flags: &["--endpoint", "--recorded", "--region", "--network-policy"],
    strict_values: true,
    repeatable_flags: &[],
    max_positionals: 0,
    passthrough: false,
    handler: seed_command,
};

/// Registration of the `docgraph` slice: its routes and help text (see
/// `crate::registry`). Added after the registry, so both ranks are
/// [`crate::registry::NEW_RANK`].
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "docgraph",
    commands: &[
        WA_REVIEW,
        NORMALIZE_CAPTURE,
        NEPTUNE_VALIDATE_INPUT,
        NEPTUNE_SEED,
        NEPTUNE_MEMORY,
    ],
    synopsis: r"  toolkit documentdb wa-review --cluster-id ID --region R [--recorded FILE]
      [--analysis-data FILE] [--output DIR] [--now TS]
  toolkit documentdb normalize-capture --capture FILE --output FILE
  toolkit neptune validate-input [KEY=VALUE...]
  toolkit neptune seed [--confirm-wipe] [--endpoint HOST] [--recorded FILE | --dry-run]
  toolkit neptune memory --operation OP --input FILE [--recorded FILE | --dry-run]
",
    description: r"  documentdb    DocumentDB Well-Architected review (#4395). wa-review runs the 41
                infrastructure and database checks over recorded AWS responses
                (--recorded) and a pre-collected analysis (--analysis-data) and
                writes wa_review_results.json and wa_review_report.md.
                normalize-capture turns a private, user-assembled mongo-shell
                capture (docs: references/collection-recipe.md) into the
                --analysis-data file; it never connects to a database and
                refuses --uri, --tls-ca-file and --tls-allow-invalid-certs.
  neptune       Neptune input validation and test-graph seeding (#4395).
                validate-input checks identifiers before a write call; seed
                wipes and seeds the test graphs only with --confirm-wipe (or
                NEPTUNE_SEED_CONFIRM_WIPE=1), replays --recorded responses or
                prints the statements with --dry-run: this build has no
                Gremlin or AWS client. memory prints or replays GraphRAG and
                agent-memory requests. See docs/migration/docgraph.md.
",
    exit_notes: "",
    doc: "docs/migration/docgraph.md",
    synopsis_rank: crate::registry::NEW_RANK,
    description_rank: crate::registry::NEW_RANK,
};

/// The value of a required value flag, when given.
fn required<'a>(invocation: &'a Invocation, flag: &str) -> Option<&'a str> {
    invocation.value(flag)
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0)
}

/// Reads and parses a JSON file named by the caller. The error text names the
/// file as [`safe_path`] does and the failure, never a value of the document
/// (`startup::read_json_document` with a redacted label).
fn read_document(path: &str) -> Result<Json, String> {
    let label = safe_path(path);
    let text = read_text(Path::new(path), &label)?;
    parse_json(&text)
        .map_err(|error| format!("{label}: invalid JSON ({})", parse_failure_text(&error)))
}

fn recorded_error(path: &str, reason: &str) -> ToolkitError {
    ToolkitError::runtime(
        "docgraph_recorded",
        format!("{}: {reason}", safe_path(path)),
    )
}

fn load_recorded(path: &str) -> Result<Json, ToolkitError> {
    read_document(path).map_err(|reason| ToolkitError::runtime("docgraph_recorded", reason))
}

/// Validate live selection before any input, policy, credential or DNS work.
fn live_selected(invocation: &Invocation) -> Result<bool, ToolkitError> {
    if !invocation.has_flag("--live") {
        return Ok(false);
    }
    if !cfg!(feature = "docgraph-live") {
        return Err(usage("--live requires the docgraph-live feature"));
    }
    if invocation.has_flag("--dry-run") || invocation.value("--recorded").is_some() {
        return Err(usage(
            "--live cannot be combined with --dry-run or --recorded",
        ));
    }
    Ok(true)
}

fn unavailable_live() -> ToolkitError {
    usage("live docgraph adapters are not available in this build")
}

/// Handler of `toolkit documentdb wa-review`.
pub fn wa_review_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    if live_selected(invocation)? {
        return Err(unavailable_live());
    }
    let missing: Vec<&str> = ["--cluster-id", "--region"]
        .into_iter()
        .filter(|flag| required(invocation, flag).is_none())
        .collect();
    if !missing.is_empty() {
        return Err(usage(format!(
            "the following arguments are required: {}",
            missing.join(", ")
        )));
    }
    let cluster_id = required(invocation, "--cluster-id").unwrap_or_default();
    let region = required(invocation, "--region").unwrap_or_default();
    if !neptune_input::identifier(cluster_id, 63) {
        return Err(usage(
            "--cluster-id must start with a letter and hold only letters, digits and hyphens (63 characters at most)",
        ));
    }
    if !is_valid_region(region) {
        return Err(usage("--region must look like us-east-1"));
    }
    let now = match invocation.value("--now") {
        None => now_seconds(),
        Some(text) => parse_iso_timestamp(text)
            .map_err(|_| usage("--now must be an ISO timestamp such as 2026-01-31T12:00:00Z"))?
            .epoch_seconds(),
    };
    let output_dir = invocation.value("--output").unwrap_or(".");

    out.line(&format!(
        "Running Well-Architected Review for {cluster_id} in {region}..."
    ))?;
    out.line("  Running infrastructure checks (AWS APIs)...")?;
    let mut recorded: Option<RecordedClient> = None;
    if let Some(path) = invocation.value("--recorded") {
        let document = load_recorded(path)?;
        recorded = Some(
            RecordedClient::from_document(&document)
                .map_err(|reason| recorded_error(path, &reason))?,
        );
    } else {
        out.diagnostic(
            "note: this build includes no live AWS client; pass --recorded FILE to replay recorded responses",
        );
    }
    let mut no_live = NoLiveClient;
    let client: &mut dyn AwsClient = match recorded.as_mut() {
        Some(recorded) => recorded,
        None => &mut no_live,
    };
    let mut results = infra::run_infra_checks(client, cluster_id, now).checks;
    out.line(&format!(
        "  Infrastructure: {} checks completed",
        results.len()
    ))?;

    let mut analysis = None;
    if let Some(path) = invocation.value("--analysis-data") {
        out.line(&format!(
            "  Loading database stats from {}...",
            safe_path(path)
        ))?;
        let document = read_document(path)
            .map_err(|reason| ToolkitError::runtime("docgraph_analysis", reason))?;
        analysis = snapshot::read_analysis(&document)
            .map_err(|reason| ToolkitError::runtime("docgraph_analysis", reason))?;
    }
    if let Some(collections) = analysis {
        out.line("  Running database-level checks...")?;
        let db_results = dbchecks::run_db_checks(&collections).checks;
        out.line(&format!(
            "  Database: {} checks completed",
            db_results.len()
        ))?;
        results.extend(db_results);
    } else {
        out.line("  Skipping database-level checks (no --uri or --analysis-data)")?;
    }

    let timestamp = utc_iso(now);
    let date = format!("{} {}", &timestamp[..10], &timestamp[11..16]);
    let (json_path, md_path) = report::write_report(
        Path::new(output_dir),
        &results,
        cluster_id,
        &timestamp,
        &date,
    )
    .map_err(|reason| ToolkitError::runtime("docgraph_report", reason))?;
    out.line("")?;
    out.line(&format!("  Results: {}", safe_path(&json_path)))?;
    out.line(&format!("  Report:  {}", safe_path(&md_path)))?;

    let [pass, warn, fail, info] = report::counts(&results);
    out.line("")?;
    out.line(&format!(
        "  Total: {} checks -- {pass} pass, {warn} warn, {fail} fail, {info} info",
        results.len()
    ))
}

/// Handler of `toolkit neptune validate-input`.
pub fn validate_input_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    if invocation.positionals.is_empty() {
        out.line("Usage: toolkit neptune validate-input key=value [key=value ...]")?;
        out.line(&format!("Valid keys: {}", neptune_input::valid_keys_repr()))?;
        return Ok(());
    }
    let mut pairs: Vec<(String, String)> = Vec::new();
    for argument in &invocation.positionals {
        let Some((key, value)) = argument.split_once('=') else {
            out.line(&format!(
                "Invalid argument: {}. Use key=value format.",
                safe_text(argument)
            ))?;
            out.set_exit_status(1);
            return Ok(());
        };
        match pairs.iter_mut().find(|(existing, _)| existing == key) {
            Some(pair) => pair.1 = value.to_string(),
            None => pairs.push((key.to_string(), value.to_string())),
        }
    }
    let errors = neptune_input::validate_all(&pairs);
    if errors.is_empty() {
        return out.line("\u{2705} All validations passed.");
    }
    out.line("Validation failed:")?;
    for error in &errors {
        out.line(&format!("  \u{274c} {error}"))?;
    }
    out.set_exit_status(1);
    Ok(())
}

/// A host name for the Gremlin URL: letters, digits, dots and hyphens.
fn is_host(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

fn seed_failure(error: &SeedError) -> ToolkitError {
    let reason = match error {
        SeedError::WipeNotConfirmed => seed::WIPE_REFUSAL.to_string(),
        SeedError::Verification(reason) => (*reason).to_string(),
        SeedError::Call(toolkit_aws::AwsError::Service(_)) => {
            "seeding failed: the graph returned an error".to_string()
        }
        SeedError::Call(other) => format!("seeding failed: {other}"),
    };
    ToolkitError::runtime("docgraph_seed", reason)
}

/// Handler of `toolkit neptune seed`.
pub fn seed_command(invocation: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    if live_selected(invocation)? {
        if !invocation.has_flag("--confirm-wipe") {
            return Err(ToolkitError::runtime("docgraph_seed", "Refusing to wipe the graph: g.V().drop() requires explicit confirmation. Re-run with --confirm-wipe. Never run this against a graph holding data you need."));
        }
        if invocation.value("--endpoint").is_none() {
            return Err(usage("--endpoint is required with --live"));
        }
        return Err(unavailable_live());
    }
    let recorded = invocation.value("--recorded");
    if invocation.has_flag("--dry-run") {
        if recorded.is_some() {
            return Err(usage("--dry-run and --recorded cannot be combined"));
        }
        for line in seed::dry_run_lines() {
            out.line(&line)?;
        }
        return Ok(());
    }
    let endpoint = invocation.value("--endpoint").unwrap_or("localhost");
    if !is_host(endpoint) {
        return Err(usage(
            "--endpoint must be a host name of letters, digits, dots and hyphens",
        ));
    }
    let confirmed = invocation.has_flag("--confirm-wipe")
        || ProcessEnv.var("NEPTUNE_SEED_CONFIRM_WIPE").as_deref() == Some("1");
    let mut replay: Option<RecordedSession> = None;
    if let Some(path) = recorded {
        let document = load_recorded(path)?;
        replay = Some(
            RecordedSession::from_document(&document)
                .map_err(|reason| recorded_error(path, &reason))?,
        );
    }
    let mut no_live = NoLiveSession;
    let session: &mut dyn GremlinSession = match replay.as_mut() {
        Some(replay) => replay,
        None => &mut no_live,
    };
    let mut lines: Vec<String> = Vec::new();
    let result = seed::run(session, endpoint, confirmed, &mut |text| {
        lines.push(text.to_string());
    });
    for line in &lines {
        out.line(line)?;
    }
    result.map_err(|error| seed_failure(&error))?;
    if let Some(replay) = &replay {
        // A run that left recorded entries unused is a wrong recording.
        let unused = replay.unused();
        if unused > 0 {
            return Err(ToolkitError::runtime(
                "docgraph_recorded",
                format!("{unused} recorded call(s) were not used"),
            ));
        }
    }
    Ok(())
}

/// Handler of `toolkit documentdb normalize-capture`.
pub fn normalize_capture_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let missing: Vec<&str> = ["--capture", "--output"]
        .into_iter()
        .filter(|flag| required(invocation, flag).is_none())
        .collect();
    if !missing.is_empty() {
        return Err(usage(format!(
            "the following arguments are required: {}",
            missing.join(", ")
        )));
    }
    let capture = required(invocation, "--capture").unwrap_or_default();
    let output = required(invocation, "--output").unwrap_or_default();
    let limits = capture::Limits::default();
    let refuse = |error: capture::CaptureError| {
        ToolkitError::runtime(
            "docgraph_capture",
            format!("capture refused ({}): {}", error.class(), error.message()),
        )
    };
    let document = capture::read_capture(Path::new(capture), &limits).map_err(refuse)?;
    let normalized = capture::normalize(&document, &limits).map_err(refuse)?;
    capture::write_snapshot(Path::new(output), &normalized.snapshot).map_err(refuse)?;
    out.line(&format!(
        "Normalized {} collection(s) across {} database(s)",
        normalized.collections, normalized.databases
    ))?;
    if normalized.incomplete > 0 {
        out.line(&format!(
            "WARNING: {} of {} collection(s) incomplete (error markers; wa-review skips them)",
            normalized.incomplete, normalized.collections
        ))?;
    }
    out.line(&format!("  Analysis data: {}", safe_path(output)))
}

/// Offline memory request planning and replay.
pub fn memory_command(invocation: &Invocation, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    if live_selected(invocation)? {
        let op = invocation
            .value("--operation")
            .ok_or_else(|| usage("--operation is required"))?;
        let required: &[&str] = match op {
            "query" => &["--confirm-write", "--confirm-delete"],
            "consolidate" => &["--confirm-delete"],
            "create-graphrag" | "create-memory" => &["--confirm-create"],
            "vector-search" | "retrieve" | "semantic-recall" | "hybrid-recall"
            | "recall-entity" | "recent-messages" => &[],
            "remember-relationship"
            | "remember-entity"
            | "store-message"
            | "ingest-document"
            | "store-embedding"
            | "link-chunk-entity"
            | "link-entities"
            | "remember-embedding"
            | "ingest-entity"
            | "ingest-chunk" => &["--confirm-write"],
            _ => return Err(usage("unknown operation")),
        };
        for flag in required {
            if !invocation.has_flag(flag) {
                return Err(ToolkitError::runtime(
                    "docgraph_memory",
                    format!("live operation requires {flag}"),
                ));
            }
        }
        return Err(unavailable_live());
    }
    let op = invocation
        .value("--operation")
        .ok_or_else(|| usage("--operation is required"))?;
    let input_path = invocation
        .value("--input")
        .ok_or_else(|| usage("--input is required"))?;
    let input =
        read_document(input_path).map_err(|e| ToolkitError::runtime("docgraph_memory", e))?;
    let requests = memory::plan(op, &input).map_err(usage)?;
    if invocation.has_flag("--dry-run") {
        if invocation.value("--recorded").is_some() {
            return Err(usage("--dry-run and --recorded cannot be combined"));
        }
        return out.line(&Json::Array(requests).to_compact_string());
    }
    let path = invocation
        .value("--recorded")
        .ok_or_else(|| usage("this build has no live client; supply --dry-run or --recorded"))?;
    let document = load_recorded(path)?;
    let mut replay =
        memory::RecordedMemory::from_document(&document).map_err(|e| recorded_error(path, &e))?;
    let result = if op == "retrieve" {
        memory::retrieve(
            &mut replay,
            input
                .get("graph_id")
                .and_then(Json::as_str)
                .unwrap_or_default(),
            &requests[0],
        )
        .map_err(|e| ToolkitError::runtime("docgraph_memory", e))?
    } else {
        memory::execute(&mut replay, op, &requests)
            .map_err(|e| ToolkitError::runtime("docgraph_memory", e))?
    };
    if replay.unused() != 0 {
        return Err(ToolkitError::runtime(
            "docgraph_recorded",
            "unused recorded calls",
        ));
    }
    out.line(&result.to_compact_string())
}

/// `neptune memory` offline request builder and replay.
pub const NEPTUNE_MEMORY: CommandSpec = CommandSpec {
    path: &["neptune", "memory"],
    flags: &[
        "--dry-run",
        "--live",
        "--confirm-write",
        "--confirm-delete",
        "--confirm-create",
    ],
    value_flags: &[
        "--operation",
        "--input",
        "--recorded",
        "--region",
        "--network-policy",
    ],
    strict_values: true,
    repeatable_flags: &[],
    max_positionals: 0,
    passthrough: false,
    handler: memory_command,
};
