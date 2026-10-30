//! `toolkit dynamodb ...` commands (slice `dynamo-cost`, Issue #4389).
//!
//! - [`cost`]: port of `amazon-dynamodb/scripts/calculate_costs.py`
//!   (`toolkit dynamodb calculate-costs`).
//! - [`iterate`]: port of `amazon-dynamodb/scripts/iterate_design.py`
//!   (`toolkit dynamodb iterate-design`).
//! - [`pyfmt`]: the Python `repr`/`round`/`json.dumps` renderings both ports
//!   must keep byte-for-byte.
//!
//! The behavior contract, the parity table against the Python sources and
//! every intentional change are recorded in `docs/migration/dynamo-cost.md`.
//! Nothing here contacts AWS: the cost calculator is pure, and the AWS-facing
//! steps of the design loop run through the shared command table so they stay
//! owned by their own slices (see [`iterate`]).
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::fn_params_excessive_bools,
    clippy::float_cmp,
    clippy::implicit_hasher,
    clippy::many_single_char_names,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::struct_excessive_bools,
    clippy::struct_field_names,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::trivially_copy_pass_by_ref,
    clippy::type_complexity,
    clippy::unnecessary_wraps
)]

use std::path::{Path, PathBuf};

use toolkit_core::{parse_json, read_regular_file, write_regular_file, Json, ToolkitError};

use crate::{Invocation, Output};

pub mod bench;
pub mod bench_fmt;
pub mod bench_rows;
pub mod cost;
pub mod iterate;
pub mod perf_render;
pub mod perf_report;
pub mod pyfmt;

pub use iterate::iterate_design_command;

/// Registration of the `dynamodb` slice: its routes and help text (see `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "dynamodb",
    commands: &[
        crate::CommandSpec {
            path: &["dynamodb", "calculate-costs"],
            flags: &[],
            value_flags: CALCULATE_COSTS_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: calculate_costs_command,
        },
        crate::CommandSpec {
            path: &["dynamodb", "iterate-design"],
            flags: ITERATE_DESIGN_FLAGS,
            value_flags: ITERATE_DESIGN_VALUE_FLAGS,
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: iterate_design_command,
        },
    ],
    synopsis: r"  toolkit dynamodb calculate-costs --model FILE [--requirements FILE] [--output FILE]
  toolkit dynamodb iterate-design --model FILE --config FILE --loop-state FILE
      [--manifest FILE] [--apply-change FILE] [--mode MODE] [--timestamp TS]
      [--yes-deploy] [--allow-spend] [--skip-deploy] [--calculator-only] [--dry-run]
",
    description: r"  dynamodb calculate-costs
                Price a dynamodb_data_model.json (RRU/WRU, GSI amplification,
                storage, vector indexes) and write a cost_report.md.
  dynamodb iterate-design
                Run ONE round of the human-driven DynamoDB design loop, then stop.
",
    exit_notes: "",
    doc: "docs/migration/dynamo-cost.md",
    synopsis_rank: 220,
    description_rank: 220,
};

/// Upper bound for design, config, state and report JSON files (16 MiB).
pub const MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;

/// Value flags of `dynamodb calculate-costs`.
pub const CALCULATE_COSTS_VALUE_FLAGS: &[&str] = &["--model", "--requirements", "--output"];

/// Boolean flags of `dynamodb iterate-design`.
pub const ITERATE_DESIGN_FLAGS: &[&str] = &[
    "--yes-deploy",
    "--allow-spend",
    "--skip-deploy",
    "--calculator-only",
    "--dry-run",
];

/// Value flags of `dynamodb iterate-design`.
pub const ITERATE_DESIGN_VALUE_FLAGS: &[&str] = &[
    "--model",
    "--config",
    "--loop-state",
    "--manifest",
    "--apply-change",
    "--mode",
    "--timestamp",
];

/// The value of a required option, or the argparse-style usage error.
pub(crate) fn required_option<'a>(
    invocation: &'a Invocation,
    name: &str,
) -> Result<&'a str, ToolkitError> {
    invocation.value(name).ok_or_else(|| {
        ToolkitError::usage(
            "missing_option",
            format!("the following arguments are required: {name}"),
        )
    })
}

/// Reads and parses a JSON file through the shared path contract: symbolic
/// links and non-regular files are refused and the size is bounded.
pub(crate) fn load_json(path: &Path) -> Result<Json, ToolkitError> {
    let text = read_regular_file(path, MAX_INPUT_BYTES)?;
    Ok(parse_json(&text)?)
}

/// Writes a text file through the shared path contract.
pub(crate) fn write_text(path: &Path, contents: &str) -> Result<(), ToolkitError> {
    Ok(write_regular_file(path, contents)?)
}

/// Inputs of `dynamodb calculate-costs`.
#[derive(Debug, Clone)]
pub struct CalculateArgs {
    /// `--model`: the `dynamodb_data_model.json` to price.
    pub model: PathBuf,
    /// `--requirements`: optional requirements artifact (write-action and
    /// retention modeling).
    pub requirements: Option<PathBuf>,
    /// `--output`: report path; defaults to `cost_report.md` beside the model.
    pub output: Option<PathBuf>,
}

fn vector_fix_hint() -> &'static str {
    concat!(
        "\nFix the model — do NOT delete the vector index to get past this, and do ",
        "NOT estimate the vector cost by hand. Both drop a real cost that is often ",
        "the largest line in the estimate. The correct shape is:\n",
        "\n  \"tables\": [{\n",
        "    \"table_name\": \"Products\",\n",
        "    \"vector_indexes\": [{\n",
        "      \"index_name\": \"ProductEmbeddingIndex\",\n",
        "      \"vector_attribute\": \"Embedding\",\n",
        "      \"dimensions\": 1024,\n",
        "      \"distance_function\": \"COSINE\",\n",
        "      \"projection\": { \"type\": \"ALL\" },\n",
        "      \"search_schema\": { \"partition_key\": \"category\" }\n",
        "    }]\n",
        "  }],\n",
        "  \"access_patterns\": [{\n",
        "    \"pattern_id\": \"FindSimilarProducts\",\n",
        "    \"operation\": \"SearchVectors\",\n",
        "    \"table\": \"Products\",\n",
        "    \"index\": \"ProductEmbeddingIndex\",\n",
        "    \"peak_rps\": 60,\n",
        "    \"top_k\": 50\n",
        "  }, {\n",
        "    \"pattern_id\": \"PutProduct\",\n",
        "    \"operation\": \"PutItem\",\n",
        "    \"table\": \"Products\",\n",
        "    \"peak_rps\": 200,\n",
        "    \"attributes_written\": [\"Embedding\"]\n",
        "  }]\n",
        "\nNote `index_name` (not `name`), `index` (not `vector_index`), and that a ",
        "write storing the embedding lists it in `attributes_written`. Full schema: ",
        "references/cost-model-schema.md ('Vector index' and 'Access pattern')."
    )
}

/// The `main()` of `calculate_costs.py`: load, validate, price, write the
/// report and print the two summary lines.
pub fn run_calculate_costs(args: &CalculateArgs, out: &mut Output<'_>) -> Result<(), ToolkitError> {
    let model = load_json(&args.model).map_err(|error| {
        ToolkitError::runtime(
            "model_unreadable",
            format!("cannot load model: {}", error.message()),
        )
    })?;

    let requirements = match &args.requirements {
        Some(path) => match load_json(path) {
            Ok(value) => Some(value),
            Err(error) => {
                out.diagnostic(&format!(
                    "Warning: Could not load requirements ({}), using defaults.",
                    error.message()
                ));
                None
            }
        },
        None => None,
    };

    if !matches!(model, Json::Object(_)) {
        return Err(cost::invalid("the data model must be a JSON object"));
    }
    if !model.get("access_patterns").is_some_and(pyfmt::truthy) {
        return Err(ToolkitError::runtime(
            "no_access_patterns",
            "No access patterns found in the data model.",
        ));
    }

    // Hard-fail a malformed or impossible vector design BEFORE pricing it:
    // otherwise the vector line prices at $0 and reads as "vectors are cheap".
    let tables = match model.get("tables") {
        Some(Json::Array(items)) => items.as_slice(),
        _ => &[],
    };
    let patterns = match model.get("access_patterns") {
        Some(Json::Array(items)) => items.as_slice(),
        _ => &[],
    };
    let vector_errors = cost::validate_vector_model(tables, patterns)?;
    if !vector_errors.is_empty() {
        let mut message =
            String::from("the vector portion of this data model cannot be priced as written.");
        for problem in &vector_errors {
            message.push_str("\n  - ");
            message.push_str(problem);
        }
        message.push('\n');
        message.push_str(vector_fix_hint());
        return Err(ToolkitError::runtime("vector_model_invalid", message));
    }

    let report = cost::calculate_and_report(&model, requirements.as_ref())?;

    let output_path = args.output.clone().unwrap_or_else(|| {
        args.model
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join("cost_report.md")
    });
    write_text(&output_path, &report)?;
    out.line(&format!(
        "Cost report written to {}",
        // Redacted before normalizing hides `//` userinfo (#4941).
        pyfmt::normalized_path(Path::new(&*crate::safe_text::redacted_value(
            &output_path.display().to_string()
        )))
    ))?;
    out.line(&format!(
        "Analyzed {} access patterns across {} tables.",
        patterns.len(),
        tables.len()
    ))
}

/// Handler of `toolkit dynamodb calculate-costs`.
pub fn calculate_costs_command(
    invocation: &Invocation,
    out: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let args = CalculateArgs {
        model: PathBuf::from(required_option(invocation, "--model")?),
        requirements: invocation.value("--requirements").map(PathBuf::from),
        output: invocation.value("--output").map(PathBuf::from),
    };
    run_calculate_costs(&args, out)
}

#[cfg(test)]
mod cost_tests;
#[cfg(test)]
mod iterate_tests;
#[cfg(test)]
pub(crate) mod test_support;
