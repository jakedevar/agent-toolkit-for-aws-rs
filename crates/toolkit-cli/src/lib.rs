//! `toolkit` command routing, output and error contracts.
//!
//! The binary in `main.rs` only forwards the process arguments and the real
//! stdout/stderr to [`run`]. Everything observable lives here so it can be
//! tested in-process with injected writers as well as through the real binary:
//!
//! - [`COMMANDS`] is the routing table. A [`CommandSpec`] names the command
//!   path (`skills list`), the flags it accepts, how many positional
//!   arguments it takes and a [`Handler`].
//! - [`route`] turns arguments into a [`Routed`] decision (help, version or a
//!   handler plus an [`Invocation`]) or a usage [`ToolkitError`].
//! - [`Output`] writes stdout lines. A closed pipe (`toolkit ... | head`) is a
//!   normal stop; any other write failure becomes a runtime error that is
//!   reported on stderr with exit status 1.
//! - [`run`] maps every [`ToolkitError`] to `error: <message>` on stderr (plus
//!   the usage text for usage errors) and its exit status.
//!
//! The full user-facing contract is in `crates/toolkit-cli/README.md`.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::PathBuf;

use toolkit_core::{discover_skills, skills_to_json, ErrorKind, ToolkitError};

mod ai_data;
pub mod asm_exec;
mod aurora;
mod bedrock_report;
pub mod dynamodb;
pub mod manifests;
mod msk;
mod safe_text;
pub mod spec;
pub mod startup;
mod sync;

/// Usage text printed by `help` (stdout) and after usage errors (stderr).
pub const USAGE: &str = "\
Usage:
  toolkit skills list [ROOT] [--json]
  toolkit manifests validate [ROOT] [--plugin NAME]
  toolkit spec validate [ROOT]
  toolkit startup model-pricing [--plugin-root DIR]
  toolkit startup program-artifacts --migration-dir DIR
  toolkit startup model-id-lint [--plugin-root DIR]
  toolkit startup artifacts (--self-check | --fixtures | --run-dir DIR --skill NAME)
                            [--plugin-root DIR] [--manifest FILE] [--baseline FILE]
                            [--no-baseline] [--json]
  toolkit startup emit-plan --migration-dir DIR [--plugin-root DIR] [--plugin-json FILE]
  toolkit startup consent-audit [--plugin-root DIR]
  toolkit startup pricing-parity [--plugin-root DIR]
  toolkit startup terraform-policy DIR [--json FILE]
  toolkit startup migration-report REPORT [--estimation-infra FILE] [--estimation-ai FILE]
                            [--aws-design FILE] [--migration-dir DIR] [--no-require-toc]
                            [--no-readability] [--mode full|decision|ai_only]
  toolkit startup heroku-report REPORT [--migration-dir DIR] [--mode full|decision]
  toolkit startup score ANSWERS [--plugin-root DIR] [--runtimes-dir DIR]
  toolkit startup score-units ANSWERS [--plugin-root DIR] [--runtimes-dir DIR]
  toolkit sync plugin-skills [--root DIR] [--plugin NAME] [--check]
  toolkit sync vendored [--plugin-root DIR] [--check]
  toolkit msk size [--emit-static-pricing] [--use-max-partitions]
                   [--no-rack-affined-consumers] [--region REGION]
                   [--pricing-source auto|api|static]
                   [--avg-data-in-mbs N] [--peak-data-in-mbs N]
                   [--avg-data-out-mbs N] [--peak-data-out-mbs N]
                   [--num-partitions N] [--retention-hours N]
                   [--primary-retention-hours N] [--replication-factor N]
                   [--utilization-standard F] [--utilization-express F]
                   [--pst-per-broker-mbs N] [--discount-pct PCT]
                   [--broker-classes standard|express|standard,express]
                   [--broker-quota N]
  toolkit msk compat CLUSTER_CONFIG [--out-dir DIR]
  toolkit msk sim compute [--instance-type T] [--broker-count N]
                          [--kafka-version V]
  toolkit msk sim render --instance-type T --broker-count N
                         --kafka-version V --output PATH
  toolkit msk sim validate --test e2e_latency|broker_restart --config JSON
                           [--instance-type T] [--broker-count N]
  toolkit dynamodb calculate-costs --model FILE [--requirements FILE] [--output FILE]
  toolkit dynamodb iterate-design --model FILE --config FILE --loop-state FILE
      [--manifest FILE] [--apply-change FILE] [--mode MODE] [--timestamp TS]
      [--yes-deploy] [--allow-spend] [--skip-deploy] [--calculator-only] [--dry-run]
  toolkit aurora acu estimate --engine mysql|postgresql --instance TYPE --cpu-p95 N --cpu-max N
      [--cpu-avg N] [--connections N] [--working-set GIB] [--storage GIB]
      [--num-instances N] [--format json|table] [--region R] [--offline]
  toolkit aurora acu list-instances --engine ENGINE [--region R] [--offline]
  toolkit aurora acu pricing-source --engine ENGINE [--region R] [--offline]
  toolkit aurora commitment [offline] --engine ENGINE [--region R] [--format json|table]
      [--cluster ID | --all] [--instance TYPE] [--num-instances N] [--io-optimized]
      [--serverless --avg-acu N]
  toolkit aurora io [offline] --engine ENGINE [--region R] [--format json|table] [--days N]
      [--cluster ID | --all] [--instance TYPE] [--num-instances N]
      [--storage-gib GIB] [--monthly-io-millions N]
  toolkit ai-data detect-format PATH_OR_S3_URI [--sample-size BYTES] [--json]
  toolkit ai-data validate-metrics [JSON_OR_FILE]
  toolkit ai-data transform plan --script FILE --source-s3 URI --output-s3 URI
      [--instance-type T] [--region R] [--execution-role ARN]
      [--base-job-name N] [--image-uri URI]
  toolkit ai-data transform submit (same options as plan)
  toolkit ai-data transform describe JOB_NAME [--region R]
  toolkit bedrock-report render [PAYLOAD_JSON] [--phase-results DIR --repo DIR]
      [--date-suffix YYYY-MM-DD] [--results-dir DIR]
  toolkit bedrock-report validate (--schema analysis|ingestion|eval|rewrite|delta-decisions FILE
      | --check-run-context SAVED_JSON --current CURRENT_JSON)
  toolkit asm exec [--] COMMAND [ARG...]
  toolkit --version
  toolkit help

Commands:
  skills list   List skill directories (directories containing SKILL.md) under
                ROOT, sorted by name then relative path. ROOT defaults to
                `skills` in the current directory.
  manifests validate
                Validate the marketplace manifests, plugin manifests, MCP
                configs and SKILL.md frontmatter of the repository at ROOT
                (default: the current directory). With --plugin NAME only
                that plugin under ROOT/plugins is validated. Problems are
                reported as `  ERROR: ...` lines on stderr and exit status 1.
  spec validate
                Validate every plugin under ROOT/plugins (default ROOT: the
                current directory) against the Agent Plugins v1.0.0 schemas in
                ROOT/tools/schemas/1.0.0, the path, placeholder and MCP rules
                the schemas cannot express, and the SKILL.md frontmatter of
                each plugin skill. Problems are reported as `  ERROR: ...`
                lines on stderr and exit status 1.
  startup model-pricing
                Require a numeric Active price row in the gcp-to-aws and
                azure-to-aws Bedrock pricing caches for every default
                Anthropic candidate model of the aws-startup-advisor plugin
                at --plugin-root DIR (default: the current directory).
  startup program-artifacts
                Check that STARTUP_PROGRAMS.md, ai-migration/STARTUP_PROGRAMS.md
                and migration-report.html in the migration directory do not
                imply a confirmed AWS Activate tier when preferences.json
                records startup_program_status as unknown, and that they link
                to the Activate application page when they mention it.
  startup model-id-lint
                Fail when a file of the plugin at --plugin-root DIR (default: the current
                directory) names a Bedrock model the lifecycle registry has
                retired, outside the catalog files that record it.
  startup emit-plan
                Copy the validated cost figures of a finished GCP or Heroku
                migration run in --migration-dir DIR into plan.json for the
                AWS Startups Migrate web import (producerVersion from
                --plugin-json FILE, default: .claude-plugin/plugin.json of
                --plugin-root DIR). Fail-open: any missing or unreadable input
                prints `PLAN_SKIP | reason=...`, writes no file and exits 0;
                success prints `PLAN_OK | path=... | platform=... | scope=...`.
  startup consent-audit
                Fail when a discover-*-api.md usage-API discovery sub-file of the
                plugin at --plugin-root DIR (default: the current directory)
                carries a blanket GET-only claim although its Capture Endpoint
                Table calls another HTTP method and the claim does not name
                that method in an exception sentence.
  startup pricing-parity
                Prove the Bedrock pricing-cache split of the plugin at --plugin-root
                DIR (default: the current directory) preserved the data of the
                pre-split commit (read with git show): sections, metadata,
                embedding rows, lookup keys, representative estimates, byte-equal
                vendored copies and neutral consumers. Prints a JSON report; the
                Python-behaviour comparison of bedrock_pricing.py is not run.
  startup terraform-policy
                Static policy checks on the generated Terraform in DIR: ALB TLS
                posture, public RDS, public database and admin ports in inline
                security-group ingress, wildcard IAM, RDS and ElastiCache
                encryption. --json FILE writes the verdict. Violations are
                POLICY_FAIL lines on stderr and exit status 1.
  startup migration-report
                Validate a generated migration-report.html (or decision-report.html
                with --mode decision, or an AI-only report with --mode ai_only):
                required section ids, table-of-contents anchors, appendix depth,
                readability, accessibility, the visual shell, the data-cost-key
                figures against --estimation-infra, and the decision-core content.
                --estimation-ai and --aws-design add the checks that need those
                artifacts; --migration-dir enables fixture-bleed and what-if
                scenario checks. Problems are `  - ...` lines on stderr after
                REPORT_FAIL and exit status 1; a pass prints REPORT_OK.
  startup heroku-report
                Validate a heroku-to-aws migration-report.html (or decision-report.html
                with --mode decision): required sections, the draft-for-review
                footer, whole-dollar currency, the data-cost-key figure against
                estimation-infra.json in --migration-dir, cost-optimization content,
                the verdict headline and flip conditions, accessibility and the
                decision-mode phase state. Problems are `  - ...` lines on stderr
                after REPORT_FAIL and exit status 1; a pass prints REPORT_OK.
  startup artifacts
                Validate migration artifacts against the contracts the skills
                publish (JSON examples in the schema documents plus the JSON
                Schemas) using the manifest scripts/artifact-contracts.json of
                the plugin at --plugin-root DIR. --self-check parses every
                contract document, --fixtures checks the fixture goldens with
                the shipped baseline, --run-dir DIR --skill NAME checks one
                run (--skill is gcp-to-aws, heroku-to-aws or azure-to-aws).
                Findings and the PASS/FAIL summary go to stdout (--json for a
                report object); exit status 1 when an error finding remains.
  startup score ANSWERS
                Score the answers document ANSWERS (agent-advisor scoring.py)
                against the runtime registry of the plugin at --plugin-root DIR
                (default: the current directory; --runtimes-dir DIR names the
                registry directly), write scoring-result.json beside ANSWERS and
                print RESULT=ok VERDICT=<verdict>.
  startup score-units ANSWERS
                Score every agent_session unit of ANSWERS (agent-advisor
                score_units.py), use the run evidence
                current-run-verifications.json beside it when present, and
                print the {units, <primary unit mirror>} JSON document.
  sync plugin-skills
                Make plugins/<plugin>/skills/<name> copies byte-identical to the
                canonical skills/**/<name> (a directory holding SKILL.md). --root
                defaults to the current directory. --check reports drift, exits
                1 and writes nothing.
  sync vendored Make each skills/<skill>/references/vendored copy in the plugin
                at --plugin-root (default plugins/aws-startup-advisor)
                byte-identical to skills/shared and its README table. --check
                reports drift, exits 1 and writes nothing.
  msk size      MSK broker sizing and monthly cost report (MiB/s, GiB, USD).
                The workload options are required unless
                --emit-static-pricing is given. Only --pricing-source static
                (us-east-1) prices offline; `auto` falls back to it, `api`
                needs a build with an AWS Pricing API client.
  msk compat    Five-pillar MSK Express compatibility assessment of a
                discovery cluster-config.json. Writes
                compatibility.<cluster_name>.json into --out-dir (default:
                current directory) and reports the path and the overall
                verdict on stderr.
  msk sim       MSK Express load-test simulation: `compute` derives capacity
                and fleet sizes, `render` writes a CloudFormation template
                with the sizing baked in, `validate` returns PASS/REJECT with
                warnings for a proposed test config.
  dynamodb calculate-costs
                Price a dynamodb_data_model.json (RRU/WRU, GSI amplification,
                storage, vector indexes) and write a cost_report.md.
  dynamodb iterate-design
                Run ONE round of the human-driven DynamoDB design loop, then stop.
  aurora acu | commitment | io
                Aurora ACU sizing, Reserved Instance and Savings Plan, and
                Standard versus I/O-Optimized cost analyzers (#4388). --engine
                (mysql or postgresql) is required. `offline` prices a
                user-supplied workload; without it the command reads AWS,
                which this build cannot do (exit 2), except `acu`, which falls
                back to the built-in us-east-1 prices. Output is JSON unless
                --format table. See docs/migration/aurora.md.
  ai-data       Dataset and metric tools of the AWS AI/ML skill (#4385).
                detect-format samples the first --sample-size bytes (default
                1 MiB, at most 16 MiB) of a local JSONL file or s3:// object and
                reports one of 11 training formats with its validation errors
                (exit 1 when invalid). validate-metrics checks a custom-metric
                JSON array (inline or a file). transform builds (plan) or
                submits and describes (submit, describe) a SageMaker Processing
                job; this build has no SageMaker or S3 client, so submit and
                describe exit 1 and s3:// inputs are refused. See
                docs/migration/ai-data.md.
  bedrock-report
                Reports of the llm-to-bedrock skill (#4405). render prints the
                in-chat summary of a finished run from the phase-result files
                in --phase-results DIR (with --repo) or from one legacy
                PAYLOAD_JSON, and copies the repository's newest
                MIGRATION_REPORT_*.md to --results-dir (default
                ~/saws-migrate-results). validate checks one phase-result
                file against the schema analysis, ingestion, eval, rewrite or
                delta-decisions (RESULT=valid CONTROL=ok|blocked|partial,
                or RESULT=invalid and one line per finding, exit 1; exit 2
                for an unreadable file) or compares a saved and a current
                run context (RUN_CONTEXT=match, or mismatch and MISMATCH
                lines, exit 1). Findings never print values of the checked
                file. See docs/migration/bedrock-report.md.
  asm exec      Replace every {{resolve:secretsmanager:ID[:SecretString[:KEY
                [:STAGE]]]}} reference in COMMAND, its arguments and the
                environment with the secret value, then run COMMAND with the
                result. One optional leading `--` is dropped; every other word,
                dash-prefixed ones included, belongs to COMMAND. Exits with
                COMMAND's status; 1 when a reference cannot be resolved or
                COMMAND cannot be started (nothing is run then). See
                docs/migration/asm-exec.md.

Options:
  --json        Emit a JSON array of {name, description, path} objects.
  --plugin NAME (manifests validate) Validate only the plugin directory
                ROOT/plugins/NAME. (sync plugin-skills) Sync only that plugin.
  --check       (sync) Report drift without writing; exit 1 when out of sync.
  --root DIR, --plugin-root DIR
                (sync) Repository root and plugin root. (startup) --plugin-root
                is the plugin directory (default: .). A value may also be
                written --flag=VALUE.
  --runtimes-dir DIR
                (startup score, score-units) The runtime profile directory
                (default: skills/agent-advisor/references/runtimes of
                --plugin-root).
  --migration-dir DIR
                (startup) Migration run directory holding preferences.json.
  -h, --help    Print this help.
  -V, --version Print the version.
  Value options take `--name value` or `--name=value`; the msk commands are
  documented in crates/toolkit-cli/README.md.

Exit codes:
  0  success (including help, version and an empty result)
  1  runtime error (missing root, root not a directory, unreadable files,
     stdout write failure, unknown plugin, failed manifest or spec
     validation, or drift found by a sync --check; msk size: pricing or
     workload error; msk sim validate: decision REJECT)
  2  usage error (unknown command, missing or unknown arguments,
     or a non-UTF-8 argument; msk sim: invalid sizing, config or output,
     reported as a JSON {\"error\": ...} object on stderr)
  Some commands keep a published status instead (iterate-design: 2 for a
  missing input file or refused change, 4 for a deploy without --yes-deploy,
  or the failed step's own status); see crates/toolkit-cli/README.md.
  asm exec exits with the status of COMMAND (a command ended by signal N
  exits 256-N); its own failures exit 1, including a missing COMMAND.";

/// Buffered stdout lines with the closed-pipe and write-error contract.
pub struct Output<'a> {
    sink: &'a mut dyn Write,
    diagnostics: Option<&'a mut dyn Write>,
    closed: bool,
    status: u8,
}

impl<'a> Output<'a> {
    /// Wraps a writer.
    pub fn new(sink: &'a mut dyn Write) -> Self {
        Self {
            sink,
            diagnostics: None,
            closed: false,
            status: 0,
        }
    }

    /// Wraps a stdout writer plus a diagnostics (stderr) writer for commands
    /// that report findings line by line (#4375).
    pub fn with_diagnostics(sink: &'a mut dyn Write, diagnostics: &'a mut dyn Write) -> Self {
        Self {
            sink,
            diagnostics: Some(diagnostics),
            closed: false,
            status: 0,
        }
    }

    /// Writes `text` and a newline to the diagnostics writer, if there is one.
    ///
    /// Failures are ignored: when stderr itself is gone there is nowhere left
    /// to report that, which is also the rule for the final error report.
    pub fn diagnostic(&mut self, text: &str) {
        if let Some(writer) = self.diagnostics.as_mut() {
            let _ = writer
                .write_all(text.as_bytes())
                .and_then(|()| writer.write_all(b"\n"));
        }
    }

    /// Sets the exit status [`run`] returns when the handler itself succeeds.
    /// Commands whose published contract uses a non-zero status together with
    /// regular output (a REJECT verdict, a legacy error code) set it here (#4387).
    pub fn set_exit_status(&mut self, status: u8) {
        self.status = status;
    }

    /// The status set with [`Output::set_exit_status`] (default 0).
    #[must_use]
    pub fn exit_status(&self) -> u8 {
        self.status
    }

    /// True once the reader went away; handlers should stop producing output.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Writes `text` and a newline. After a closed pipe this is a no-op.
    ///
    /// # Errors
    ///
    /// Returns a runtime error (`stdout_write`) for any write failure other
    /// than a closed pipe, which is treated as normal completion.
    pub fn line(&mut self, text: &str) -> Result<(), ToolkitError> {
        if self.closed {
            return Ok(());
        }
        let result = self
            .sink
            .write_all(text.as_bytes())
            .and_then(|()| self.sink.write_all(b"\n"));
        self.settle(result)
    }

    /// Flushes buffered output with the same error contract as [`Output::line`].
    ///
    /// # Errors
    ///
    /// Same as [`Output::line`].
    pub fn finish(&mut self) -> Result<(), ToolkitError> {
        if self.closed {
            return Ok(());
        }
        let result = self.sink.flush();
        self.settle(result)
    }

    fn settle(&mut self, result: io::Result<()>) -> Result<(), ToolkitError> {
        match result {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                self.closed = true;
                Ok(())
            }
            Err(error) => {
                self.closed = true;
                Err(ToolkitError::runtime(
                    "stdout_write",
                    format!("cannot write to stdout: {error}"),
                ))
            }
        }
    }
}

/// Parsed arguments handed to a command handler.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// Positional arguments in order.
    pub positionals: Vec<String>,
    /// Flags that were present (each at most once).
    pub flags: Vec<String>,
    /// Value flags that were present with their value (the last one wins).
    pub values: Vec<(String, String)>,
}

impl Invocation {
    /// Whether `name` (for example `--json`) was passed.
    #[must_use]
    pub fn has_flag(&self, name: &str) -> bool {
        self.flags.iter().any(|flag| flag == name)
    }

    /// The value of the value flag `name` (for example `--plugin`; the last one
    /// when repeated).
    #[must_use]
    pub fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .rev()
            .find(|(flag, _)| flag == name)
            .map(|(_, value)| value.as_str())
    }

    /// The positional argument at `index`, if any.
    #[must_use]
    pub fn positional(&self, index: usize) -> Option<&str> {
        self.positionals.get(index).map(String::as_str)
    }

    fn set_value(&mut self, name: &str, value: &str) {
        if let Some(slot) = self.values.iter_mut().find(|(flag, _)| flag == name) {
            slot.1 = value.to_string();
        } else {
            self.values.push((name.to_string(), value.to_string()));
        }
    }
}

/// A command implementation.
pub type Handler = fn(&Invocation, &mut Output<'_>) -> Result<(), ToolkitError>;

/// One routable command.
#[derive(Clone, Copy)]
pub struct CommandSpec {
    /// Words that select the command, for example `["skills", "list"]`.
    pub path: &'static [&'static str],
    /// Long flags the command accepts, for example `["--json"]`.
    pub flags: &'static [&'static str],
    /// Long flags that take one value, written `--name VALUE` or
    /// `--name=VALUE`. A missing value is a usage error; see `strict_values`.
    pub value_flags: &'static [&'static str],
    /// Whether a value flag rejects an empty value and one starting with `-`
    /// (so `--plugin-root --check` is a usage error). Off for
    /// `manifests validate`, whose `--plugin` takes the next word whatever it
    /// looks like (#4375); on for `sync` (#4377).
    pub strict_values: bool,
    /// Maximum number of positional arguments.
    pub max_positionals: usize,
    /// Whether every word after the command path is a positional argument,
    /// verbatim: dash-prefixed words, `--` and `-h`/`--help` are not
    /// interpreted and `max_positionals` is ignored. For commands that run
    /// another program (`asm exec`, #4451) whose own flags must reach it.
    pub passthrough: bool,
    /// The implementation.
    pub handler: Handler,
}

impl fmt::Debug for CommandSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandSpec")
            .field("path", &self.path)
            .field("flags", &self.flags)
            .field("value_flags", &self.value_flags)
            .finish_non_exhaustive()
    }
}

/// The routing table. New domain slices register their commands here.
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        path: &["skills", "list"],
        flags: &["--json"],
        value_flags: &[],
        strict_values: false,
        max_positionals: 1,
        passthrough: false,
        handler: skills_list,
    },
    CommandSpec {
        path: &["manifests", "validate"],
        flags: &[],
        value_flags: &["--plugin"],
        strict_values: false,
        max_positionals: 1,
        passthrough: false,
        handler: manifests::validate_command,
    },
    CommandSpec {
        path: &["spec", "validate"],
        flags: &[],
        value_flags: &[],
        strict_values: false,
        max_positionals: 1,
        passthrough: false,
        handler: spec::validate_command,
    },
    CommandSpec {
        path: &["startup", "model-pricing"],
        flags: &[],
        value_flags: &["--plugin-root"],
        strict_values: false,
        max_positionals: 0,
        passthrough: false,
        handler: startup::pricing_command,
    },
    CommandSpec {
        path: &["startup", "program-artifacts"],
        flags: &[],
        value_flags: &["--migration-dir"],
        strict_values: false,
        max_positionals: 0,
        passthrough: false,
        handler: startup::program_artifacts_command,
    },
    CommandSpec {
        path: &["startup", "model-id-lint"],
        flags: &[],
        value_flags: &["--plugin-root"],
        strict_values: false,
        max_positionals: 0,
        passthrough: false,
        handler: startup::model_id_lint_command,
    },
    CommandSpec {
        path: &["startup", "artifacts"],
        flags: &["--self-check", "--fixtures", "--no-baseline", "--json"],
        value_flags: &[
            "--run-dir",
            "--skill",
            "--manifest",
            "--baseline",
            "--plugin-root",
        ],
        strict_values: false,
        max_positionals: 0,
        passthrough: false,
        handler: startup::artifacts_command,
    },
    CommandSpec {
        path: &["startup", "emit-plan"],
        flags: &[],
        value_flags: &["--migration-dir", "--plugin-root", "--plugin-json"],
        strict_values: false,
        max_positionals: 0,
        passthrough: false,
        handler: startup::emit_plan_command,
    },
    CommandSpec {
        path: &["startup", "consent-audit"],
        flags: &[],
        value_flags: &["--plugin-root"],
        strict_values: false,
        max_positionals: 0,
        passthrough: false,
        handler: startup::consent_audit_command,
    },
    CommandSpec {
        path: &["startup", "pricing-parity"],
        flags: &[],
        value_flags: &["--plugin-root"],
        strict_values: false,
        max_positionals: 0,
        passthrough: false,
        handler: startup::pricing_parity_command,
    },
    CommandSpec {
        path: &["startup", "terraform-policy"],
        flags: &[],
        value_flags: &["--json"],
        strict_values: false,
        max_positionals: 1,
        passthrough: false,
        handler: startup::terraform_policy_command,
    },
    CommandSpec {
        path: &["startup", "migration-report"],
        flags: &["--no-require-toc", "--no-readability"],
        value_flags: &[
            "--estimation-infra",
            "--estimation-ai",
            "--aws-design",
            "--migration-dir",
            "--mode",
        ],
        strict_values: false,
        max_positionals: 1,
        passthrough: false,
        handler: startup::migration_report_command,
    },
    CommandSpec {
        path: &["startup", "heroku-report"],
        flags: &[],
        value_flags: &["--migration-dir", "--mode"],
        strict_values: true,
        max_positionals: 1,
        passthrough: false,
        handler: startup::heroku_report_command,
    },
    CommandSpec {
        path: &["startup", "score"],
        flags: &[],
        value_flags: &["--plugin-root", "--runtimes-dir"],
        strict_values: false,
        max_positionals: 1,
        passthrough: false,
        handler: startup::score_command,
    },
    CommandSpec {
        path: &["startup", "score-units"],
        flags: &[],
        value_flags: &["--plugin-root", "--runtimes-dir"],
        strict_values: false,
        max_positionals: 1,
        passthrough: false,
        handler: startup::score_units_command,
    },
    CommandSpec {
        path: &["sync", "plugin-skills"],
        flags: &["--check"],
        value_flags: &["--root", "--plugin"],
        strict_values: true,
        max_positionals: 0,
        passthrough: false,
        handler: sync::plugin_skills,
    },
    CommandSpec {
        path: &["sync", "vendored"],
        flags: &["--check"],
        value_flags: &["--plugin-root"],
        strict_values: true,
        max_positionals: 0,
        passthrough: false,
        handler: sync::vendored,
    },
    CommandSpec {
        path: &["msk", "size"],
        flags: msk::SIZE_FLAGS,
        value_flags: msk::SIZE_VALUE_FLAGS,
        strict_values: false,
        max_positionals: 0,
        passthrough: false,
        handler: msk::size,
    },
    CommandSpec {
        path: &["msk", "compat"],
        flags: &[],
        value_flags: msk::COMPAT_VALUE_FLAGS,
        strict_values: false,
        max_positionals: 1,
        passthrough: false,
        handler: msk::compat,
    },
    CommandSpec {
        path: &["msk", "sim", "compute"],
        flags: &[],
        value_flags: msk::SIM_COMPUTE_VALUE_FLAGS,
        strict_values: false,
        max_positionals: 0,
        passthrough: false,
        handler: msk::sim_compute,
    },
    CommandSpec {
        path: &["msk", "sim", "render"],
        flags: &[],
        value_flags: msk::SIM_RENDER_VALUE_FLAGS,
        strict_values: false,
        max_positionals: 0,
        passthrough: false,
        handler: msk::sim_render,
    },
    CommandSpec {
        path: &["msk", "sim", "validate"],
        flags: &[],
        value_flags: msk::SIM_VALIDATE_VALUE_FLAGS,
        strict_values: false,
        max_positionals: 0,
        passthrough: false,
        handler: msk::sim_validate,
    },
    CommandSpec {
        path: &["dynamodb", "calculate-costs"],
        flags: &[],
        value_flags: dynamodb::CALCULATE_COSTS_VALUE_FLAGS,
        strict_values: true,
        max_positionals: 0,
        passthrough: false,
        handler: dynamodb::calculate_costs_command,
    },
    CommandSpec {
        path: &["dynamodb", "iterate-design"],
        flags: dynamodb::ITERATE_DESIGN_FLAGS,
        value_flags: dynamodb::ITERATE_DESIGN_VALUE_FLAGS,
        strict_values: true,
        max_positionals: 0,
        passthrough: false,
        handler: dynamodb::iterate_design_command,
    },
    CommandSpec {
        path: &["aurora", "acu", "estimate"],
        flags: &["--offline"],
        value_flags: &[
            "--engine",
            "--region",
            "--instance",
            "--cpu-p95",
            "--cpu-max",
            "--cpu-avg",
            "--connections",
            "--working-set",
            "--storage",
            "--num-instances",
            "--format",
        ],
        strict_values: false,
        max_positionals: 0,
        passthrough: false,
        handler: aurora::acu_estimate_command,
    },
    CommandSpec {
        path: &["aurora", "acu", "list-instances"],
        flags: &["--offline"],
        value_flags: &["--engine", "--region"],
        strict_values: false,
        max_positionals: 0,
        passthrough: false,
        handler: aurora::acu_list_instances_command,
    },
    CommandSpec {
        path: &["aurora", "acu", "pricing-source"],
        flags: &["--offline"],
        value_flags: &["--engine", "--region"],
        strict_values: false,
        max_positionals: 0,
        passthrough: false,
        handler: aurora::acu_pricing_source_command,
    },
    // `aurora acu` alone prints the help; it must follow the longer `acu` paths.
    CommandSpec {
        path: &["aurora", "acu"],
        flags: &["--offline"],
        value_flags: &["--engine", "--region"],
        strict_values: false,
        max_positionals: 0,
        passthrough: false,
        handler: aurora::acu_help_command,
    },
    CommandSpec {
        path: &["aurora", "commitment"],
        flags: &["--all", "--serverless", "--io-optimized"],
        value_flags: &[
            "--engine",
            "--region",
            "--format",
            "--cluster",
            "--instance",
            "--num-instances",
            "--avg-acu",
        ],
        strict_values: false,
        max_positionals: 1,
        passthrough: false,
        handler: aurora::commitment_command,
    },
    CommandSpec {
        path: &["aurora", "io"],
        flags: &["--all"],
        value_flags: &[
            "--engine",
            "--region",
            "--format",
            "--days",
            "--cluster",
            "--instance",
            "--num-instances",
            "--storage-gib",
            "--monthly-io-millions",
        ],
        strict_values: false,
        max_positionals: 1,
        passthrough: false,
        handler: aurora::io_command,
    },
    CommandSpec {
        path: &["ai-data", "detect-format"],
        flags: ai_data::DETECT_FORMAT_FLAGS,
        value_flags: ai_data::DETECT_FORMAT_VALUE_FLAGS,
        strict_values: true,
        max_positionals: 1,
        passthrough: false,
        handler: ai_data::detect_format_command,
    },
    CommandSpec {
        path: &["ai-data", "validate-metrics"],
        flags: &[],
        value_flags: &[],
        strict_values: false,
        max_positionals: 1,
        passthrough: false,
        handler: ai_data::validate_metrics_command,
    },
    CommandSpec {
        path: &["ai-data", "transform", "plan"],
        flags: &[],
        value_flags: ai_data::TRANSFORM_VALUE_FLAGS,
        strict_values: true,
        max_positionals: 0,
        passthrough: false,
        handler: ai_data::transform_plan_command,
    },
    CommandSpec {
        path: &["ai-data", "transform", "submit"],
        flags: &[],
        value_flags: ai_data::TRANSFORM_VALUE_FLAGS,
        strict_values: true,
        max_positionals: 0,
        passthrough: false,
        handler: ai_data::transform_submit_command,
    },
    CommandSpec {
        path: &["ai-data", "transform", "describe"],
        flags: &[],
        value_flags: ai_data::DESCRIBE_VALUE_FLAGS,
        strict_values: true,
        max_positionals: 1,
        passthrough: false,
        handler: ai_data::transform_describe_command,
    },
    CommandSpec {
        path: &["bedrock-report", "render"],
        flags: &[],
        value_flags: bedrock_report::RENDER_VALUE_FLAGS,
        strict_values: true,
        max_positionals: 1,
        passthrough: false,
        handler: bedrock_report::render_command,
    },
    CommandSpec {
        path: &["bedrock-report", "validate"],
        flags: &[],
        value_flags: bedrock_report::VALIDATE_VALUE_FLAGS,
        strict_values: true,
        max_positionals: 1,
        passthrough: false,
        handler: bedrock_report::validate_command,
    },
    CommandSpec {
        path: &["asm", "exec"],
        flags: &[],
        value_flags: &[],
        strict_values: false,
        max_positionals: usize::MAX,
        passthrough: true,
        handler: asm_exec::exec_command,
    },
];

/// Outcome of routing the arguments.
#[derive(Debug)]
pub enum Routed<'a> {
    /// Print the usage text on stdout.
    Help,
    /// Print the version on stdout.
    Version,
    /// Run a command.
    Run(&'a CommandSpec, Invocation),
}

fn usage_error() -> ToolkitError {
    ToolkitError::usage("invalid_arguments", "invalid arguments")
}

/// Whether `value` may follow a value flag of `spec` (see
/// [`CommandSpec::strict_values`]).
fn value_allowed(spec: &CommandSpec, value: &str) -> bool {
    !spec.strict_values || !(value.is_empty() || value.starts_with('-'))
}

/// Routes already-decoded arguments (without the program name).
///
/// `help`/`-h`/`--help` and `version`/`-V`/`--version` as the first argument
/// win over everything after them. Inside a command, `-h`/`--help` selects
/// help, any other `-`-prefixed word must be one of the command's flags or
/// value flags and there may be at most `max_positionals` other words. A value
/// flag takes the next word (or the text after `=`) as its value; repeating it
/// keeps the last value, and a value flag at the end of the arguments with no
/// value is a usage error.
///
/// # Errors
///
/// Returns a usage error (exit status 2) for no arguments, an unknown
/// command, an unknown flag or too many positional arguments.
pub fn route<'a>(specs: &'a [CommandSpec], args: &[String]) -> Result<Routed<'a>, ToolkitError> {
    let Some(first) = args.first().map(String::as_str) else {
        return Err(usage_error());
    };
    match first {
        "-h" | "--help" | "help" => return Ok(Routed::Help),
        "-V" | "--version" | "version" => return Ok(Routed::Version),
        _ => {}
    }
    let Some(spec) = specs.iter().find(|spec| {
        args.len() >= spec.path.len()
            && spec
                .path
                .iter()
                .zip(args)
                .all(|(expected, actual)| *expected == actual.as_str())
    }) else {
        return Err(usage_error());
    };

    let mut invocation = Invocation::default();
    if spec.passthrough {
        invocation.positionals = args[spec.path.len()..].to_vec();
        return Ok(Routed::Run(spec, invocation));
    }
    let mut rest = args[spec.path.len()..].iter();
    while let Some(arg) = rest.next() {
        let word = arg.as_str();
        if word == "-h" || word == "--help" {
            return Ok(Routed::Help);
        }
        if word.starts_with('-') {
            if spec.value_flags.contains(&word) {
                // `--name value`: the next word is the value.
                let Some(value) = rest.next() else {
                    return Err(usage_error());
                };
                if !value_allowed(spec, value) {
                    return Err(usage_error());
                }
                invocation.set_value(word, value);
            } else if let Some((name, value)) = word
                .split_once('=')
                .filter(|(name, _)| spec.value_flags.contains(name))
            {
                if !value_allowed(spec, value) {
                    return Err(usage_error());
                }
                invocation.set_value(name, value);
            } else if !spec.flags.contains(&word) {
                return Err(usage_error());
            } else if !invocation.has_flag(word) {
                invocation.flags.push(arg.clone());
            }
        } else {
            if invocation.positionals.len() >= spec.max_positionals {
                return Err(usage_error());
            }
            invocation.positionals.push(arg.clone());
        }
    }
    Ok(Routed::Run(spec, invocation))
}

fn skills_list(invocation: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
    let root = PathBuf::from(invocation.positional(0).unwrap_or("skills"));
    let entries = discover_skills(&root)?;
    if invocation.has_flag("--json") {
        return output.line(&skills_to_json(&entries));
    }
    for entry in &entries {
        output.line(&entry.plain_line())?;
        if output.is_closed() {
            break;
        }
    }
    Ok(())
}

/// Writes the error report and returns the exit status.
fn report(err: &mut dyn Write, error: &ToolkitError) -> u8 {
    let text = match error.kind() {
        ErrorKind::Usage => format!("error: {}\n\n{USAGE}\n", error.message()),
        ErrorKind::Runtime => format!("error: {}\n", error.message()),
    };
    // stderr may be closed too; there is nowhere left to report that.
    let _ = err.write_all(text.as_bytes());
    error.exit_code()
}

fn print_static(out: &mut dyn Write, err: &mut dyn Write, text: &str) -> u8 {
    let mut output = Output::new(out);
    let result = output.line(text).and_then(|()| output.finish());
    match result {
        Ok(()) => 0,
        Err(error) => report(err, &error),
    }
}

/// Runs the CLI with the given arguments (without the program name) and
/// writers, returning the process exit status.
pub fn run(args: &[OsString], out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let mut text_args = Vec::with_capacity(args.len());
    for arg in args {
        let Some(text) = arg.to_str() else {
            return report(
                err,
                &ToolkitError::usage(
                    "invalid_arguments",
                    "invalid arguments: arguments must be valid UTF-8",
                ),
            );
        };
        text_args.push(text.to_string());
    }
    match route(COMMANDS, &text_args) {
        Ok(Routed::Help) => print_static(out, err, USAGE),
        Ok(Routed::Version) => {
            print_static(out, err, &format!("toolkit {}", env!("CARGO_PKG_VERSION")))
        }
        Ok(Routed::Run(spec, invocation)) => {
            let mut output = Output::with_diagnostics(out, err);
            let result = (spec.handler)(&invocation, &mut output).and_then(|()| output.finish());
            match result {
                Ok(()) => output.exit_status(),
                Err(error) => report(err, &error),
            }
        }
        Err(error) => report(err, &error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "toolkit-cli-lib-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock after epoch")
                    .as_nanos()
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn skill(root: &Path, relative: &str, content: &str) {
        let full = root.join(relative);
        fs::create_dir_all(&full).expect("create skill dir");
        fs::write(full.join("SKILL.md"), content).expect("write SKILL.md");
    }

    fn words(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn os_args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    /// Runs the CLI in-process; returns (status, stdout, stderr).
    fn exec(values: &[&str]) -> (u8, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let status = run(&os_args(values), &mut out, &mut err);
        (
            status,
            String::from_utf8(out).expect("utf-8 stdout"),
            String::from_utf8(err).expect("utf-8 stderr"),
        )
    }

    /// Writer that fails every call with the given kind.
    struct FailingWriter(io::ErrorKind);

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(self.0))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(self.0))
        }
    }

    fn route_words(values: &[&str]) -> Result<Routed<'static>, ToolkitError> {
        route(COMMANDS, &words(values))
    }

    #[test]
    fn routes_skills_list_with_flags_and_root_in_any_order() {
        for values in [
            vec!["skills", "list", "somewhere", "--json"],
            vec!["skills", "list", "--json", "somewhere"],
        ] {
            let Ok(Routed::Run(spec, invocation)) = route_words(&values) else {
                panic!("expected a runnable route for {values:?}");
            };
            assert_eq!(spec.path, &["skills", "list"]);
            assert_eq!(invocation.positional(0), Some("somewhere"));
            assert!(invocation.has_flag("--json"));
        }
    }

    #[test]
    fn duplicate_flags_are_collapsed_and_defaults_are_empty() {
        let Ok(Routed::Run(_, invocation)) = route_words(&["skills", "list", "--json", "--json"])
        else {
            panic!("expected a runnable route");
        };
        assert_eq!(invocation.flags, vec!["--json".to_string()]);
        assert_eq!(invocation.positional(0), None);
        assert!(!Invocation::default().has_flag("--json"));
    }

    #[test]
    fn value_flags_take_one_value_in_either_spelling() {
        for values in [
            vec![
                "sync",
                "plugin-skills",
                "--root",
                "r",
                "--plugin",
                "p",
                "--check",
            ],
            vec!["sync", "plugin-skills", "--check", "--plugin=p", "--root=r"],
        ] {
            let Ok(Routed::Run(spec, invocation)) = route_words(&values) else {
                panic!("expected a runnable route for {values:?}");
            };
            assert_eq!(spec.path, &["sync", "plugin-skills"]);
            assert_eq!(invocation.value("--root"), Some("r"));
            assert_eq!(invocation.value("--plugin"), Some("p"));
            assert!(invocation.has_flag("--check"));
            assert!(invocation.positionals.is_empty());
        }
        let Ok(Routed::Run(_, invocation)) =
            route_words(&["sync", "vendored", "--plugin-root", "a", "--plugin-root=b"])
        else {
            panic!("expected a runnable route");
        };
        assert_eq!(
            invocation.value("--plugin-root"),
            Some("b"),
            "last one wins"
        );
        assert_eq!(invocation.value("--root"), None);
    }

    #[test]
    fn value_flag_misuse_is_a_usage_error() {
        for values in [
            vec!["sync", "vendored", "--plugin-root"],
            vec!["sync", "vendored", "--plugin-root", "--check"],
            vec!["sync", "vendored", "--plugin-root", ""],
            vec!["sync", "vendored", "--plugin-root="],
            vec!["sync", "vendored", "--plugin-root=-x"],
            vec!["sync", "vendored", "--check=1"],
            vec!["sync", "vendored", "--root", "x"],
            vec!["sync", "vendored", "positional"],
            vec!["sync", "plugin-skills", "--json"],
            vec!["skills", "list", "--root", "x"],
            vec!["sync"],
        ] {
            let Err(error) = route_words(&values) else {
                panic!("{values:?} must be a usage error");
            };
            assert_eq!(error.kind(), ErrorKind::Usage, "{values:?}");
            assert_eq!(error.exit_code(), 2);
        }
    }

    #[test]
    fn help_and_version_short_circuit() {
        for values in [
            vec!["--help"],
            vec!["-h"],
            vec!["help"],
            vec!["help", "ignored", "--wat"],
            vec!["skills", "list", "-h"],
            vec!["skills", "list", "--help"],
        ] {
            assert!(
                matches!(route_words(&values), Ok(Routed::Help)),
                "{values:?}"
            );
        }
        for values in [vec!["--version"], vec!["-V"], vec!["version", "x"]] {
            assert!(
                matches!(route_words(&values), Ok(Routed::Version)),
                "{values:?}"
            );
        }
    }

    #[test]
    fn usage_errors_are_exit_two_with_a_stable_code() {
        for values in [
            vec![],
            vec!["unknown"],
            vec!["skills"],
            vec!["skills", "export"],
            vec!["skills", "list", "--wat"],
            vec!["skills", "list", "-"],
            vec!["skills", "list", "a", "b"],
            vec!["list"],
            vec!["--json"],
        ] {
            let Err(error) = route_words(&values) else {
                panic!("{values:?} must be a usage error");
            };
            assert_eq!(error.kind(), ErrorKind::Usage, "{values:?}");
            assert_eq!(error.code(), "invalid_arguments");
            assert_eq!(error.exit_code(), 2);
        }
    }

    #[test]
    fn registry_commands_are_documented_in_usage() {
        for spec in COMMANDS {
            let joined = spec.path.join(" ");
            assert!(USAGE.contains(&joined), "usage lacks '{joined}'");
            for flag in spec.flags.iter().chain(spec.value_flags) {
                assert!(USAGE.contains(flag), "usage lacks '{flag}'");
            }
        }
        let paths: Vec<&[&str]> = COMMANDS.iter().map(|spec| spec.path).collect();
        for (index, path) in paths.iter().enumerate() {
            assert!(!paths[..index].contains(path), "duplicate route {path:?}");
        }
    }

    #[test]
    fn passthrough_commands_take_every_word_verbatim() {
        let routed = route_words(&["asm", "exec", "--", "ls", "-la", "--help", "-h", "x=1"]);
        let Ok(Routed::Run(spec, invocation)) = routed else {
            panic!("asm exec must route to its handler");
        };
        assert_eq!(spec.path, ["asm", "exec"]);
        assert_eq!(
            invocation.positionals,
            words(&["--", "ls", "-la", "--help", "-h", "x=1"])
        );
        assert!(invocation.flags.is_empty() && invocation.values.is_empty());
        // No words at all still routes (the handler prints the usage line).
        assert!(matches!(
            route_words(&["asm", "exec"]),
            Ok(Routed::Run(_, invocation)) if invocation.positionals.is_empty()
        ));
        // Other commands still reject unknown flags.
        assert!(route_words(&["skills", "list", "--bogus"]).is_err());
    }

    #[test]
    fn a_handler_can_return_a_status_without_an_error_report() {
        let (status, stdout, stderr) = exec(&["asm", "exec"]);
        assert_eq!(
            (status, stdout.as_str(), stderr.as_str()),
            (1, "", "Usage: asm-exec <command> [args...]\n")
        );
        #[cfg(unix)]
        {
            let (status, stdout, stderr) = exec(&["asm", "exec", "/bin/sh", "-c", "exit 3"]);
            assert_eq!((status, stdout.as_str(), stderr.as_str()), (3, "", ""));
        }
    }

    #[test]
    fn help_and_version_write_to_stdout_only() {
        let (status, stdout, stderr) = exec(&["--help"]);
        assert_eq!((status, stderr.as_str()), (0, ""));
        assert_eq!(stdout, format!("{USAGE}\n"));
        let (status, stdout, stderr) = exec(&["-V"]);
        assert_eq!((status, stderr.as_str()), (0, ""));
        assert_eq!(stdout, format!("toolkit {}\n", env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn usage_error_report_has_message_then_usage_on_stderr() {
        let (status, stdout, stderr) = exec(&["bogus"]);
        assert_eq!(status, 2);
        assert_eq!(stdout, "");
        assert_eq!(stderr, format!("error: invalid arguments\n\n{USAGE}\n"));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_arguments_are_usage_errors() {
        use std::os::unix::ffi::OsStringExt;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let status = run(
            &[OsString::from_vec(b"skills\xff".to_vec())],
            &mut out,
            &mut err,
        );
        assert_eq!(status, 2);
        assert!(out.is_empty());
        let stderr = String::from_utf8(err).expect("utf-8 stderr");
        assert!(stderr.starts_with("error: invalid arguments: arguments must be valid UTF-8"));
        assert!(stderr.contains("Usage:"));
    }

    #[test]
    fn skills_list_prints_plain_and_json_in_sorted_order() {
        let root = TempDir::new("list");
        skill(root.path(), "z/zulu", "---\ndescription: last\n---\n");
        skill(
            root.path(),
            "a/alpha",
            "---\nname: alpha\ndescription: first\n---\n",
        );
        let root_arg = root.path().to_str().expect("utf-8 temp path");

        let (status, stdout, stderr) = exec(&["skills", "list", root_arg]);
        assert_eq!((status, stderr.as_str()), (0, ""));
        assert_eq!(stdout, "alpha\tfirst\nzulu\tlast\n");

        let (status, stdout, stderr) = exec(&["skills", "list", root_arg, "--json"]);
        assert_eq!((status, stderr.as_str()), (0, ""));
        assert_eq!(
            stdout,
            "[\n  {\n    \"name\": \"alpha\",\n    \"description\": \"first\",\n    \"path\": \"a/alpha\"\n  },\n  {\n    \"name\": \"zulu\",\n    \"description\": \"last\",\n    \"path\": \"z/zulu\"\n  }\n]\n"
        );
    }

    #[test]
    fn runtime_errors_exit_one_with_message_only() {
        let root = TempDir::new("missing");
        let missing = root.path().join("absent");
        let (status, stdout, stderr) =
            exec(&["skills", "list", missing.to_str().expect("utf-8 temp path")]);
        assert_eq!(status, 1);
        assert_eq!(stdout, "");
        assert!(stderr.starts_with("error: root does not exist: "));
        assert!(!stderr.contains("Usage:"), "runtime errors omit usage text");
    }

    #[test]
    fn closed_pipe_is_success_and_stops_output() {
        let root = TempDir::new("pipe");
        skill(root.path(), "one", "---\nname: one\n---\n");
        let mut sink = FailingWriter(io::ErrorKind::BrokenPipe);
        let mut err = Vec::new();
        let args = os_args(&["skills", "list", root.path().to_str().expect("utf-8 path")]);
        assert_eq!(run(&args, &mut sink, &mut err), 0);
        assert!(err.is_empty(), "a closed pipe prints nothing on stderr");

        let json_args = os_args(&[
            "skills",
            "list",
            root.path().to_str().expect("utf-8 path"),
            "--json",
        ]);
        assert_eq!(run(&json_args, &mut sink, &mut err), 0);
        assert!(err.is_empty());
    }

    #[test]
    fn other_stdout_write_errors_are_reported_with_exit_one() {
        let root = TempDir::new("write-error");
        skill(root.path(), "one", "---\nname: one\n---\n");
        let mut sink = FailingWriter(io::ErrorKind::PermissionDenied);
        let mut err = Vec::new();
        let args = os_args(&["skills", "list", root.path().to_str().expect("utf-8 path")]);
        assert_eq!(run(&args, &mut sink, &mut err), 1);
        let stderr = String::from_utf8(err).expect("utf-8 stderr");
        assert!(
            stderr.starts_with("error: cannot write to stdout: "),
            "{stderr}"
        );
        assert!(!stderr.contains("Usage:"));

        // Help goes through the same contract.
        let mut err = Vec::new();
        let mut sink = FailingWriter(io::ErrorKind::PermissionDenied);
        assert_eq!(run(&os_args(&["--help"]), &mut sink, &mut err), 1);
        assert!(String::from_utf8(err)
            .expect("utf-8 stderr")
            .contains("cannot write to stdout"));
    }

    #[test]
    fn dynamodb_value_flags_route_with_the_canonical_router() {
        let Ok(Routed::Run(spec, invocation)) = route_words(&[
            "dynamodb",
            "calculate-costs",
            "--model",
            "a.json",
            "--output=x.md",
            "--output=y.md",
        ]) else {
            panic!("expected a runnable route");
        };
        assert_eq!(spec.path, &["dynamodb", "calculate-costs"]);
        assert_eq!(invocation.value("--model"), Some("a.json"));
        assert_eq!(invocation.value("--output"), Some("y.md"), "last wins");
        assert_eq!(invocation.value("--absent"), None);
        assert!(invocation.positionals.is_empty() && invocation.flags.is_empty());
        for values in [
            vec!["dynamodb", "calculate-costs", "--model"],
            vec!["dynamodb", "calculate-costs", "--model", "--output", "x"],
            vec!["dynamodb", "calculate-costs", "-o", "x"],
            vec!["dynamodb", "calculate-costs", "--req", "x"],
            vec!["dynamodb", "calculate-costs", "--model", "a.json", "extra"],
            vec!["dynamodb", "iterate-design", "--model=a", "--dry-run=1"],
        ] {
            let Err(error) = route_words(&values) else {
                panic!("{values:?} must be a usage error");
            };
            assert_eq!(error.exit_code(), 2, "{values:?}");
        }
        let Ok(Routed::Run(_, invocation)) = route_words(&[
            "dynamodb",
            "iterate-design",
            "--dry-run",
            "--model",
            "m",
            "--yes-deploy",
        ]) else {
            panic!("expected a runnable route");
        };
        assert!(invocation.has_flag("--dry-run") && invocation.has_flag("--yes-deploy"));
        assert_eq!(invocation.value("--model"), Some("m"));
    }

    #[test]
    fn diagnostics_reach_stderr_even_when_the_command_succeeds() {
        let root = TempDir::new("diagnostics");
        fs::write(
            root.path().join("model.json"),
            r#"{"tables":[],"access_patterns":[{"pattern_id":"P","operation":"GetItem"}]}"#,
        )
        .expect("write model");
        let model = root.path().join("model.json");
        let missing = root.path().join("absent.json");
        let report = root.path().join("report.md");
        let (status, stdout, stderr) = exec(&[
            "dynamodb",
            "calculate-costs",
            "--model",
            model.to_str().expect("utf-8 path"),
            "--requirements",
            missing.to_str().expect("utf-8 path"),
            "--output",
            report.to_str().expect("utf-8 path"),
        ]);
        assert_eq!(status, 0, "{stderr}");
        assert_eq!(
            stderr,
            format!(
                "Warning: Could not load requirements (path does not exist: {}), using defaults.\n",
                missing.display()
            )
        );
        assert_eq!(
            stdout,
            format!(
                "Cost report written to {}\nAnalyzed 1 access patterns across 0 tables.\n",
                report.display()
            )
        );
        assert!(report.exists());
    }

    #[test]
    fn output_stays_closed_after_a_failure() {
        let mut sink = FailingWriter(io::ErrorKind::PermissionDenied);
        let mut output = Output::new(&mut sink);
        assert!(output.line("first").is_err());
        assert!(output.is_closed());
        assert!(output.line("second").is_ok(), "later lines are dropped");
        assert!(output.finish().is_ok());
    }

    #[test]
    fn msk_commands_route_with_value_options_in_both_spellings() {
        let Ok(Routed::Run(spec, invocation)) = route_words(&[
            "msk",
            "sim",
            "compute",
            "--instance-type",
            "a",
            "--broker-count=9",
            "--instance-type",
            "b",
        ]) else {
            panic!("expected a runnable route");
        };
        assert_eq!(spec.path, &["msk", "sim", "compute"]);
        assert_eq!(invocation.value("--instance-type"), Some("b"));
        assert_eq!(invocation.value("--broker-count"), Some("9"));
        assert_eq!(invocation.value("--kafka-version"), None);
        assert_eq!(invocation.values.len(), 2);
    }

    #[test]
    fn msk_value_options_consume_the_next_word_even_when_it_looks_like_a_flag() {
        // argparse semantics: a negative number is a value (the legacy script
        // then rejects it), so msk uses the lenient value rule.
        let Ok(Routed::Run(spec, invocation)) = route_words(&[
            "msk",
            "size",
            "--avg-data-in-mbs",
            "-5",
            "--use-max-partitions",
        ]) else {
            panic!("expected a runnable route");
        };
        assert!(!spec.strict_values);
        assert_eq!(invocation.value("--avg-data-in-mbs"), Some("-5"));
        assert!(invocation.has_flag("--use-max-partitions"));
        assert!(invocation.positionals.is_empty());
    }

    #[test]
    fn msk_argument_errors_are_usage_errors() {
        for values in [
            vec!["msk", "sim", "compute", "--broker-count"],
            vec!["msk", "sim", "compute", "--wat", "1"],
            vec!["msk", "sim", "compute", "extra"],
            vec!["msk", "size", "--json"],
            vec!["msk", "compat", "a", "b"],
            vec!["msk"],
            vec!["msk", "sim"],
            vec!["skills", "list", "--root", "x"],
        ] {
            let Err(error) = route_words(&values) else {
                panic!("{values:?} must be a usage error");
            };
            assert_eq!(error.kind(), ErrorKind::Usage, "{values:?}");
            assert_eq!(error.exit_code(), 2);
        }
    }

    #[test]
    fn handlers_can_write_stderr_lines_and_set_the_exit_status() {
        fn handler(_: &Invocation, output: &mut Output<'_>) -> Result<(), ToolkitError> {
            output.line("data")?;
            output.diagnostic("warning one");
            output.diagnostic("warning two");
            output.set_exit_status(7);
            Ok(())
        }
        let spec = CommandSpec {
            path: &["demo"],
            flags: &[],
            value_flags: &[],
            strict_values: false,
            max_positionals: 0,
            passthrough: false,
            handler,
        };
        let Ok(Routed::Run(spec, invocation)) =
            route(std::slice::from_ref(&spec), &words(&["demo"]))
        else {
            panic!("expected a runnable route");
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut output = Output::with_diagnostics(&mut out, &mut err);
        (spec.handler)(&invocation, &mut output).expect("handler succeeds");
        assert_eq!(output.exit_status(), 7);
        assert_eq!(out, b"data\n");
        assert_eq!(err, b"warning one\nwarning two\n");
    }

    #[test]
    fn existing_command_families_still_route_next_to_msk() {
        for values in [
            vec!["manifests", "validate", "--plugin", "p"],
            vec!["spec", "validate"],
            vec!["sync", "plugin-skills", "--check", "--root", "r"],
            vec!["sync", "vendored", "--plugin-root=x"],
        ] {
            assert!(
                matches!(route_words(&values), Ok(Routed::Run(..))),
                "{values:?}"
            );
        }
        let Err(error) = route_words(&["sync", "vendored", "--plugin-root", "--check"]) else {
            panic!("strict sync values must stay strict");
        };
        assert_eq!(error.exit_code(), 2);
    }
}
