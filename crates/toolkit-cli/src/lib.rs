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

use toolkit_core::{
    discover_skills, neutralize_text_knowing, skills_to_json, ErrorKind, ToolkitError,
};

mod ai_data;
mod ai_models;
pub mod asm_exec;
mod aurora;
mod bedrock_agent;
mod bedrock_report;
mod bedrock_resolve;
pub mod cloudwatch;
pub mod dynamodb;
pub mod elasticache;
pub mod manifests;
mod msk;
mod relational;
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
  toolkit startup model-recommend INPUT [--output FILE] [--catalog FILE] [--openai-catalog FILE]
                            [--plugin-root DIR]
  toolkit startup build-diagram RESULT CONFIRM [DESIGN]
  toolkit startup verify-model-path RECOMMENDATION [--output FILE] [--workload ID[,ID...]]
  toolkit startup skill-refs (--check | --list) [--plugin-root DIR] [--baseline FILE]
                            [--no-baseline] [--json]
  toolkit startup model-staleness [--plugin-root DIR] [--strict]
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
  toolkit ai-models list-models HUB [REGION] [--list-values] [--create-snapshot]
      [--snapshot-file FILE] [--recorded FILE]
  toolkit ai-models filter MODELS_JSON [KEY:VALUE...]
  toolkit ai-models search SageMakerPublicHub [KEY:VALUE...] [--region R]
      [--snapshot-file FILE] [--recorded FILE]
  toolkit ai-models model-names HUB [REGION] [--recorded FILE]
  toolkit ai-models recipes MODEL HUB [--recorded FILE]
  toolkit ai-models endpoint-diagnostics ENDPOINT REGION [--minutes N | --start TS [--end TS]]
      [--now TS] [--recorded FILE]
  toolkit bedrock-agent fetch --agent-id ID --out FILE [--agent-version VERSION]
      [--agent-alias-id ID] [--region REGION] [--profile PROFILE]
      [--inline-s3-schemas] [--recorded FILE]
  toolkit bedrock-report render [PAYLOAD_JSON] [--phase-results DIR --repo DIR]
      [--date-suffix YYYY-MM-DD] [--results-dir DIR]
  toolkit bedrock-report validate (--schema analysis|ingestion|eval|rewrite|delta-decisions FILE
      | --check-run-context SAVED_JSON --current CURRENT_JSON)
  toolkit cloudwatch render RENDERER (--json TEXT | --json-file FILE)
  toolkit cloudwatch validate CHECK (--json TEXT | --json-file FILE)
  toolkit cloudwatch region [--region REGION]
  toolkit elasticache price [--engine valkey|redis|memcached] [--mode serverless|node]
      [--region R] [--data-gb N] [--ecpu-millions N] [--ops-per-sec N]
      [--node-type T] [--nodes N] [--ri-term TERM] [--avg-payload-kb N]
      [--show-ri-options] [--extended-support] [--interactive]
      (--pricing-csv FILE | --pricing-json FILE)
  toolkit elasticache estimate-serverless --input FILE [--commandstats FILE]
      [--output FILE] [--json] [--endpoint HOST]
      (--pricing FILE | --pricing-json FILE)
  toolkit elasticache validate-input [KEY=VALUE...]
  toolkit elasticache validate-references [--skill-dir DIR] [--verbose] [--quiet]
  toolkit elasticache preflight --host HOST [--port N] [--snapshot FILE] [--json]
  toolkit elasticache audit (--serverless NAME | --replication-group ID)
      [--describe FILE] [--json]
  toolkit elasticache dashboard (--serverless NAME | --replication-group ID)
      [--node-ids A,B] [--region R] [--sns-topic ARN] [--no-dashboard] [--no-alarms]
      [--max-storage-gb N] [--storage-alarm-pct N] [--memory-threshold N]
      [--engine-cpu-threshold N] [--replication-lag-threshold N]
      [--hit-rate-threshold N] [--throttle-threshold N]
      [--read-latency-threshold N] [--write-latency-threshold N]
      [--ecpu-threshold N] [--evictions-threshold N]
      [--new-connections-threshold N] [--output FILE]
  toolkit bedrock-resolve identity --provider anthropic|openai
      (--source MODEL --target BEDROCK_ID | --mappings FILE)
  toolkit bedrock-resolve image --emit format|mime|data-url|converse|responses|chat
      [--image PATH] [--prompt TEXT | --prompt-file FILE] [--detail low|high|auto|original]
  toolkit bedrock-resolve pricing --region REGION --models ID[,ID...] [--price-list FILE]
  toolkit bedrock-resolve source-model ENV_FILE --catalog FILE
      [--plan-model-id ID] [--source-provider NAME]
  toolkit dsql selftest
  toolkit dsql tenant-sql --tenant SLUG
  toolkit dsql tenant-query --tenant SLUG [--recorded FILE]
  toolkit rds commitment [offline] [--region R] [--format json] [--instance ID]
      [--instance-type TYPE] [--engine mysql|mariadb|postgres] [--num-instances N]
      [--multi-az] [--recorded FILE]
  toolkit rds-oracle test-connection ENDPOINT PORT SERVICE_NAME USERNAME [--verbose]
      [--recorded FILE]
  toolkit timestream-influxdb validate-input [KEY=VALUE...]
  toolkit timestream-influxdb instance-types [recommend WRITES QUERIES DAYS]
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
  startup model-recommend INPUT
                Recommend a Bedrock model and API path for every workload of
                INPUT (agent-advisor model_recommendation.py), validating INPUT
                and the result against the shipped schemas. Writes
                model-recommendation.json beside INPUT (or --output FILE),
                removes a stale model-verification.json beside it when the
                invocation contract changed, and prints RESULT=ok WORKLOADS=n
                SCHEMA_VALIDATED=yes. The dated catalogs are read from
                skills/agent-advisor/references/models of --plugin-root
                (default: the current directory) unless --catalog FILE
                (Anthropic) or --openai-catalog FILE names them.
  startup build-diagram RESULT CONFIRM [DESIGN]
                Compose the architecture diagram of the scoring result RESULT
                (agent-advisor build_diagram.py): the runtime, model and
                AgentCore services, or the multi-unit topology of the design
                DESIGN when it has several units. CONFIRM and DESIGN are
                treated as absent when the file does not exist. Writes
                diagram.md (a Mermaid block plus an ASCII fallback) beside
                RESULT and prints RESULT=ok RUNTIME=<runtime>. Text from the
                documents is escaped; no renderer is run.
  startup verify-model-path RECOMMENDATION
                Probe the model paths of a model-recommendation.json
                (agent-advisor verify_model_path.py) and write
                model-verification.json beside it (--output FILE overrides).
                --workload ID[,ID...] probes only those workloads. Prints
                RESULT=ok|failed WORKLOADS=<n>; exit 2 when a probe failed or
                needs resolution. Probe errors are redacted. This build has no
                live Bedrock client: a probe reports a RuntimeError.
  startup skill-refs (--check | --list)
                Check that every file path the aws-startup-advisor skills and
                agents mention exists (check-skill-refs.py). --check lists each
                missing reference as `ERROR  MISSING_REF  file:line  `token``,
                each baseline entry that matched nothing as STALE_BASELINE and a
                final PASS or FAIL count line; exit 1 on any problem. --list
                prints every reference and where it resolved. --baseline FILE
                (default tools/skill-refs-baseline.json of --plugin-root; a
                missing file is ignored) holds `ignore` rules and known dead
                `entries`; --no-baseline drops it; --json prints one JSON report.
                Symbolic links are refused. --plugin-root defaults to the current
                directory.
  startup model-staleness
                Check the Bedrock model-lifecycle registry
                skills/shared/ai/ai-model-lifecycle.md of --plugin-root: its
                `**Last updated:**` and `**Staleness window:**` contract, the
                required vendored copies and byte-identical vendored copies
                (model-staleness.py). A drifted or missing copy, or a missing,
                malformed or future-dated contract, fails always; a snapshot
                older than its window warns, and fails (exit 1) with --strict.
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
  ai-models     Model discovery and endpoint diagnostics of the AWS AI/ML skill
                (#4384). list-models lists a SageMaker Hub's models (or their
                distinct values, or a catalog snapshot), filter applies
                KEY:VALUE constraints to a models file, search resolves them
                against a snapshot and queries SageMaker Search (SageMakerPublicHub
                only) with a visible ListHubContents fallback, model-names lists
                the fine-tunable models, recipes the fine-tuning techniques of
                one model, endpoint-diagnostics the status, metrics and recent
                logs of an endpoint. This build has no live AWS client and sends
                nothing: --recorded FILE replays recorded responses. See
                docs/migration/ai-models.md.
  bedrock-agent fetch
                Bedrock Agent discovery (#4383): fetch the agent, its action
                groups, knowledge bases and data sources, collaborators,
                aliases, versions and execution role into one manifest JSON file
                (owner-only permissions, a link at --out is refused). This build
                has no live AWS client: --recorded FILE replays recorded
                responses, otherwise the first call fails with exit 1.
                Prints Wrote FILE and one WARNING line per discovery warning;
                no region exits 2. See docs/migration/bedrock-fetch.md.
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
  cloudwatch render | validate | region
                Offline CloudWatch dynamic-instrumentation logic (#4380): render
                a response or status report, check an input, or resolve the
                region. render RENDERER is one of location, create-success,
                list, get, batch-delete, status-history, status-assessment,
                snapshot-search, snapshot-sample, snapshot-fields, failure;
                validate CHECK is one of instrumentation-type, location-inputs,
                capture-names, probe, location-hash, snapshot-signal. The
                arguments are one JSON object (legacy parameter names) given
                inline with --json or from a file with --json-file. A failed
                check prints the legacy ERROR text on stdout and exits 1; an
                unknown or mistyped argument exits 2. Nothing contacts AWS.
                See docs/migration/cw-domain.md.
  elasticache   ElastiCache cost, validation, audit and dashboard tools
                (#4392). `price` estimates serverless and node-based cost,
                reserved options and Extended Support surcharges;
                `estimate-serverless` prices provisioned clusters from a
                cluster CSV (optionally per-command stats); `validate-input`
                checks identifiers before they reach a CLI command;
                `validate-references` checks the skill's reference files;
                `preflight` checks a Redis/Valkey source snapshot and `audit`
                recorded describe responses; `dashboard` writes a
                CloudFormation observability template. Prices are read from
                --pricing-csv / --pricing-json (the AWS bulk pricing file):
                this build has no AWS or Redis client. See
                docs/migration/elasticache.md.
  bedrock-resolve
                Model identity, image, pricing and source-model resolution of
                the llm-to-bedrock skill (#4403), offline. identity prints
                {\"same_model\": bool} for a source model and a Bedrock id (or
                {\"same_model_family\": bool} for a mappings file). image builds
                a Converse, Responses or Chat Completions user message, a data
                URL or the Converse format of an image file. pricing prints
                the per-1K-token USD rates of the listed model ids from the
                built-in table (available: false for anything unpriced; a
                recorded Pricing API answer may be given with --price-list).
                source-model resolves PLAN_MODEL_ID (or --plan-model-id)
                against a recorded provider catalog (--catalog), picking the
                same line only for a date or pure version suffix: exit 2
                {\"status\": \"no_key\"}, exit 3 {\"status\": \"error\"}. Recorded
                inputs replace the live provider and Pricing API calls. See
                docs/migration/bedrock-resolve.md.
  asm exec      Replace every {{resolve:secretsmanager:ID[:SecretString[:KEY
                [:STAGE]]]}} reference in COMMAND, its arguments and the
                environment with the secret value, then run COMMAND with the
                result. One optional leading `--` is dropped; every other word,
                dash-prefixed ones included, belongs to COMMAND. Exits with
                COMMAND's status; 1 when a reference cannot be resolved or
                COMMAND cannot be started (nothing is run then). See
                docs/migration/asm-exec.md.
  dsql selftest | tenant-sql | tenant-query
                Aurora DSQL safe SQL helpers (#4394). selftest runs the
                safe_query checks; tenant-sql prints the SELECT for a tenant
                slug (lowercase letters, digits, hyphen, 1-64 characters);
                tenant-query runs it on a read-only cursor and prints the rows
                as JSON. This build has no live connection: tenant-query needs
                --recorded FILE (recorded rows).
  rds commitment
                RDS Reserved Instance and Database Savings Plan estimator
                (#4394, read-only; no purchase API). `offline` prices
                --instance-type and --engine; otherwise --instance ID is looked
                up. Prints JSON. This build has no live AWS client:
                --recorded FILE replays recorded responses, otherwise only
                the bundled on-demand prices are used.
  rds-oracle test-connection
                Oracle connection report for an RDS instance (#4394). Endpoint,
                DSN and user are hidden unless --verbose. This build opens no
                connection and reads no password: --recorded FILE replays a
                recorded session, otherwise the run fails at the driver check
                with exit 1.
  timestream-influxdb validate-input | instance-types
                Timestream for InfluxDB input rules (instance_name,
                instance_type, region, vpc_id, subnet_id, sg_id, snapshot_id,
                storage_gb; exit 1 on a bad value) and the instance type
                table with its sizing recommendation. See
                docs/migration/relational.md.

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
  --output FILE, --catalog FILE, --openai-catalog FILE
                (startup model-recommend) The result file (default:
                model-recommendation.json beside INPUT) and the Anthropic and
                OpenAI model catalogs (default: the dated files in
                skills/agent-advisor/references/models of --plugin-root).
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
    /// Argument values that contain a newline: escaped wherever a message
    /// echoes them verbatim (see [`neutralize_text_knowing`]).
    untrusted: Vec<String>,
}

impl<'a> Output<'a> {
    /// Wraps a writer.
    pub fn new(sink: &'a mut dyn Write) -> Self {
        Self {
            sink,
            diagnostics: None,
            closed: false,
            status: 0,
            untrusted: Vec::new(),
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
            untrusted: Vec::new(),
        }
    }

    /// Registers the command-line values to escape wherever a diagnostic echoes
    /// them verbatim (only values with a newline matter).
    pub fn set_untrusted(&mut self, args: &[String]) {
        self.untrusted = untrusted_values(args);
    }

    /// Writes `text` and a newline to the diagnostics writer, if there is one.
    ///
    /// Failures are ignored: when stderr itself is gone there is nowhere left
    /// to report that, which is also the rule for the final error report.
    ///
    /// Control characters other than newline (ESC, BEL, CR, tab), bidirectional
    /// overrides and similar invisible characters are escaped by
    /// [`neutralize_text_knowing`] so echoed caller text cannot drive the terminal
    /// (#4639); newlines stay because some diagnostics are multi-line blocks, except
    /// inside a command-line value that itself contains a newline (see
    /// [`Output::set_untrusted`]), which is escaped wherever it is echoed verbatim.
    /// A handler that echoes other untrusted text must quote it itself. Safe text
    /// is written unchanged.
    pub fn diagnostic(&mut self, text: &str) {
        if let Some(writer) = self.diagnostics.as_mut() {
            let _ = writer
                .write_all(neutralize_text_knowing(text, &self.untrusted).as_bytes())
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
    /// Value flags that were present with their value: one entry per plain value
    /// flag (the last one wins), one entry per occurrence, in command-line order,
    /// for a [`CommandSpec::repeatable_flags`] flag.
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

    /// Every value of the value flag `name` in command-line order: one for a
    /// plain value flag (its last), one per occurrence for a
    /// [`CommandSpec::repeatable_flags`] flag, none when absent.
    pub fn values_of<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.values
            .iter()
            .filter(move |(flag, _)| flag == name)
            .map(|(_, value)| value.as_str())
    }

    fn store_value(&mut self, spec: &CommandSpec, name: &str, value: &str) {
        if spec.repeatable_flags.contains(&name) {
            self.values.push((name.to_string(), value.to_string()));
        } else {
            self.set_value(name, value);
        }
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
    /// Long flags that take one value like `value_flags` (same spellings, same
    /// `strict_values` check on every value) but may be repeated, argparse
    /// `action="append"` style: every occurrence is kept in command-line order
    /// and read with [`Invocation::values_of`]. A flag listed here is a value
    /// flag without also being listed in `value_flags`; listed in both, it is
    /// repeatable. A plain `value_flags` flag keeps only its last value (#4670).
    pub repeatable_flags: &'static [&'static str],
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
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: skills_list,
    },
    CommandSpec {
        path: &["manifests", "validate"],
        flags: &[],
        value_flags: &["--plugin"],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: manifests::validate_command,
    },
    CommandSpec {
        path: &["spec", "validate"],
        flags: &[],
        value_flags: &[],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: spec::validate_command,
    },
    CommandSpec {
        path: &["startup", "model-pricing"],
        flags: &[],
        value_flags: &["--plugin-root"],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: startup::pricing_command,
    },
    CommandSpec {
        path: &["startup", "program-artifacts"],
        flags: &[],
        value_flags: &["--migration-dir"],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: startup::program_artifacts_command,
    },
    CommandSpec {
        path: &["startup", "model-id-lint"],
        flags: &[],
        value_flags: &["--plugin-root"],
        strict_values: false,
        repeatable_flags: &[],
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
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: startup::artifacts_command,
    },
    CommandSpec {
        path: &["startup", "emit-plan"],
        flags: &[],
        value_flags: &["--migration-dir", "--plugin-root", "--plugin-json"],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: startup::emit_plan_command,
    },
    CommandSpec {
        path: &["startup", "consent-audit"],
        flags: &[],
        value_flags: &["--plugin-root"],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: startup::consent_audit_command,
    },
    CommandSpec {
        path: &["startup", "pricing-parity"],
        flags: &[],
        value_flags: &["--plugin-root"],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: startup::pricing_parity_command,
    },
    CommandSpec {
        path: &["startup", "terraform-policy"],
        flags: &[],
        value_flags: &["--json"],
        strict_values: false,
        repeatable_flags: &[],
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
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: startup::migration_report_command,
    },
    CommandSpec {
        path: &["startup", "heroku-report"],
        flags: &[],
        value_flags: &["--migration-dir", "--mode"],
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: startup::heroku_report_command,
    },
    CommandSpec {
        path: &["startup", "score"],
        flags: &[],
        value_flags: &["--plugin-root", "--runtimes-dir"],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: startup::score_command,
    },
    CommandSpec {
        path: &["startup", "score-units"],
        flags: &[],
        value_flags: &["--plugin-root", "--runtimes-dir"],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: startup::score_units_command,
    },
    CommandSpec {
        path: &["startup", "model-recommend"],
        flags: &[],
        value_flags: &["--output", "--catalog", "--openai-catalog", "--plugin-root"],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: startup::model_recommend_command,
    },
    CommandSpec {
        path: &["startup", "build-diagram"],
        flags: &[],
        value_flags: &[],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 3,
        passthrough: false,
        handler: startup::build_diagram_command,
    },
    CommandSpec {
        path: &["startup", "verify-model-path"],
        flags: &[],
        value_flags: &["--output", "--workload"],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: startup::verify_model_path_command,
    },
    CommandSpec {
        path: &["startup", "skill-refs"],
        flags: &["--check", "--list", "--json", "--no-baseline"],
        value_flags: &["--plugin-root", "--baseline"],
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: startup::skill_refs_command,
    },
    CommandSpec {
        path: &["startup", "model-staleness"],
        flags: &["--strict"],
        value_flags: &["--plugin-root"],
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: startup::model_staleness_command,
    },
    CommandSpec {
        path: &["sync", "plugin-skills"],
        flags: &["--check"],
        value_flags: &["--root", "--plugin"],
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: sync::plugin_skills,
    },
    CommandSpec {
        path: &["sync", "vendored"],
        flags: &["--check"],
        value_flags: &["--plugin-root"],
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: sync::vendored,
    },
    CommandSpec {
        path: &["msk", "size"],
        flags: msk::SIZE_FLAGS,
        value_flags: msk::SIZE_VALUE_FLAGS,
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: msk::size,
    },
    CommandSpec {
        path: &["msk", "compat"],
        flags: &[],
        value_flags: msk::COMPAT_VALUE_FLAGS,
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: msk::compat,
    },
    CommandSpec {
        path: &["msk", "sim", "compute"],
        flags: &[],
        value_flags: msk::SIM_COMPUTE_VALUE_FLAGS,
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: msk::sim_compute,
    },
    CommandSpec {
        path: &["msk", "sim", "render"],
        flags: &[],
        value_flags: msk::SIM_RENDER_VALUE_FLAGS,
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: msk::sim_render,
    },
    CommandSpec {
        path: &["msk", "sim", "validate"],
        flags: &[],
        value_flags: msk::SIM_VALIDATE_VALUE_FLAGS,
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: msk::sim_validate,
    },
    CommandSpec {
        path: &["dynamodb", "calculate-costs"],
        flags: &[],
        value_flags: dynamodb::CALCULATE_COSTS_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: dynamodb::calculate_costs_command,
    },
    CommandSpec {
        path: &["dynamodb", "iterate-design"],
        flags: dynamodb::ITERATE_DESIGN_FLAGS,
        value_flags: dynamodb::ITERATE_DESIGN_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
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
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: aurora::acu_estimate_command,
    },
    CommandSpec {
        path: &["aurora", "acu", "list-instances"],
        flags: &["--offline"],
        value_flags: &["--engine", "--region"],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: aurora::acu_list_instances_command,
    },
    CommandSpec {
        path: &["aurora", "acu", "pricing-source"],
        flags: &["--offline"],
        value_flags: &["--engine", "--region"],
        strict_values: false,
        repeatable_flags: &[],
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
        repeatable_flags: &[],
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
        repeatable_flags: &[],
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
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: aurora::io_command,
    },
    CommandSpec {
        path: &["ai-data", "detect-format"],
        flags: ai_data::DETECT_FORMAT_FLAGS,
        value_flags: ai_data::DETECT_FORMAT_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: ai_data::detect_format_command,
    },
    CommandSpec {
        path: &["ai-data", "validate-metrics"],
        flags: &[],
        value_flags: &[],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: ai_data::validate_metrics_command,
    },
    CommandSpec {
        path: &["ai-data", "transform", "plan"],
        flags: &[],
        value_flags: ai_data::TRANSFORM_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: ai_data::transform_plan_command,
    },
    CommandSpec {
        path: &["ai-data", "transform", "submit"],
        flags: &[],
        value_flags: ai_data::TRANSFORM_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: ai_data::transform_submit_command,
    },
    CommandSpec {
        path: &["ai-data", "transform", "describe"],
        flags: &[],
        value_flags: ai_data::DESCRIBE_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: ai_data::transform_describe_command,
    },
    CommandSpec {
        path: &["ai-models", "list-models"],
        flags: ai_models::LIST_MODELS_FLAGS,
        value_flags: ai_models::LIST_MODELS_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 2,
        passthrough: false,
        handler: ai_models::list_models_command,
    },
    CommandSpec {
        path: &["ai-models", "filter"],
        flags: &[],
        value_flags: &[],
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: ai_models::MAX_CONSTRAINT_POSITIONALS,
        passthrough: false,
        handler: ai_models::filter_command,
    },
    CommandSpec {
        path: &["ai-models", "search"],
        flags: &[],
        value_flags: ai_models::SEARCH_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: ai_models::MAX_CONSTRAINT_POSITIONALS,
        passthrough: false,
        handler: ai_models::search_command,
    },
    CommandSpec {
        path: &["ai-models", "model-names"],
        flags: &[],
        value_flags: ai_models::RECORDED_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 2,
        passthrough: false,
        handler: ai_models::model_names_command,
    },
    CommandSpec {
        path: &["ai-models", "recipes"],
        flags: &[],
        value_flags: ai_models::RECORDED_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 2,
        passthrough: false,
        handler: ai_models::recipes_command,
    },
    CommandSpec {
        path: &["ai-models", "endpoint-diagnostics"],
        flags: &[],
        value_flags: ai_models::DIAGNOSTICS_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 2,
        passthrough: false,
        handler: ai_models::endpoint_diagnostics_command,
    },
    CommandSpec {
        path: &["bedrock-agent", "fetch"],
        flags: bedrock_agent::FETCH_FLAGS,
        value_flags: bedrock_agent::FETCH_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: bedrock_agent::fetch_command,
    },
    CommandSpec {
        path: &["bedrock-report", "render"],
        flags: &[],
        value_flags: bedrock_report::RENDER_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: bedrock_report::render_command,
    },
    CommandSpec {
        path: &["bedrock-report", "validate"],
        flags: &[],
        value_flags: bedrock_report::VALIDATE_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: bedrock_report::validate_command,
    },
    CommandSpec {
        path: &["cloudwatch", "render"],
        flags: &[],
        value_flags: cloudwatch::ARGUMENT_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: cloudwatch::render_command,
    },
    CommandSpec {
        path: &["cloudwatch", "validate"],
        flags: &[],
        value_flags: cloudwatch::ARGUMENT_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: cloudwatch::validate_command,
    },
    CommandSpec {
        path: &["cloudwatch", "region"],
        flags: &[],
        value_flags: cloudwatch::REGION_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: cloudwatch::region_command,
    },
    CommandSpec {
        path: &["elasticache", "price"],
        flags: elasticache::PRICE_FLAGS,
        value_flags: elasticache::PRICE_VALUE_FLAGS,
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: elasticache::price_command,
    },
    CommandSpec {
        path: &["elasticache", "estimate-serverless"],
        flags: elasticache::ESTIMATE_FLAGS,
        value_flags: elasticache::ESTIMATE_VALUE_FLAGS,
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: elasticache::estimate_serverless_command,
    },
    CommandSpec {
        path: &["elasticache", "validate-input"],
        flags: &[],
        value_flags: &[],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: usize::MAX,
        passthrough: false,
        handler: elasticache::validate_input_command,
    },
    CommandSpec {
        path: &["elasticache", "validate-references"],
        flags: elasticache::REFERENCES_FLAGS,
        value_flags: elasticache::REFERENCES_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: elasticache::validate_references_command,
    },
    CommandSpec {
        path: &["elasticache", "preflight"],
        flags: elasticache::PREFLIGHT_FLAGS,
        value_flags: elasticache::PREFLIGHT_VALUE_FLAGS,
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: elasticache::preflight_command,
    },
    CommandSpec {
        path: &["elasticache", "audit"],
        flags: elasticache::AUDIT_FLAGS,
        value_flags: elasticache::AUDIT_VALUE_FLAGS,
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: elasticache::audit_command,
    },
    CommandSpec {
        path: &["elasticache", "dashboard"],
        flags: elasticache::DASHBOARD_FLAGS,
        value_flags: elasticache::DASHBOARD_VALUE_FLAGS,
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: elasticache::dashboard_command,
    },
    CommandSpec {
        path: &["bedrock-resolve", "identity"],
        flags: &[],
        value_flags: bedrock_resolve::IDENTITY_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: bedrock_resolve::identity_command,
    },
    CommandSpec {
        path: &["bedrock-resolve", "image"],
        flags: &[],
        value_flags: bedrock_resolve::IMAGE_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: bedrock_resolve::image_command,
    },
    CommandSpec {
        path: &["bedrock-resolve", "pricing"],
        flags: &[],
        value_flags: bedrock_resolve::PRICING_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: bedrock_resolve::pricing_command,
    },
    CommandSpec {
        path: &["bedrock-resolve", "source-model"],
        flags: &[],
        value_flags: bedrock_resolve::SOURCE_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: bedrock_resolve::source_command,
    },
    CommandSpec {
        path: &["asm", "exec"],
        flags: &[],
        value_flags: &[],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: usize::MAX,
        passthrough: true,
        handler: asm_exec::exec_command,
    },
    CommandSpec {
        path: &["dsql", "selftest"],
        flags: &[],
        value_flags: &[],
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: relational::dsql_selftest_command,
    },
    CommandSpec {
        path: &["dsql", "tenant-sql"],
        flags: &[],
        value_flags: relational::TENANT_SQL_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: relational::dsql_tenant_sql_command,
    },
    CommandSpec {
        path: &["dsql", "tenant-query"],
        flags: &[],
        value_flags: relational::TENANT_QUERY_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: relational::dsql_tenant_query_command,
    },
    CommandSpec {
        path: &["rds", "commitment"],
        flags: relational::RDS_FLAGS,
        value_flags: relational::RDS_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: relational::rds_commitment_command,
    },
    CommandSpec {
        path: &["rds-oracle", "test-connection"],
        flags: relational::ORACLE_FLAGS,
        value_flags: relational::ORACLE_VALUE_FLAGS,
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 4,
        passthrough: false,
        handler: relational::oracle_test_connection_command,
    },
    CommandSpec {
        path: &["timestream-influxdb", "validate-input"],
        flags: &[],
        value_flags: &[],
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 64,
        passthrough: false,
        handler: relational::timestream_validate_input_command,
    },
    CommandSpec {
        path: &["timestream-influxdb", "instance-types"],
        flags: &[],
        value_flags: &[],
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 4,
        passthrough: false,
        handler: relational::timestream_instance_types_command,
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

/// The pseudo-flag naming a command's positional arguments in [`STDIO_DASH`].
pub const STDIO_DASH_POSITIONAL: &str = "<positional>";

/// The (command path, flag) pairs that accept the exact token `-` as the
/// argparse stdin/stdout marker (`--json -`, #4743, #4780).
///
/// Fail closed: `-` is accepted ONLY where a pair is listed here, as the value
/// of the named value flag (`--f -` and `--f=-`) or, for the pseudo-flag
/// [`STDIO_DASH_POSITIONAL`], as a positional. The command path is the
/// space-joined [`CommandSpec::path`] (for example
/// `"cloudwatch validate instrumentation-type"`). Everywhere else `-` is
/// refused exactly as it was before #4743: a positional `-` is an unknown
/// flag, and on a `strict_values` command `-` is a refused dash-prefixed
/// value (a command without `strict_values` keeps taking any value verbatim,
/// as it always did).
///
/// A command that is not listed never receives a `-` it could mistake for a
/// stream: most slices turn a path argument straight into a filesystem path,
/// where an accepted `-` would silently read or overwrite a file named `-`.
/// A slice that wants stdin/stdout support adds its pair here in the same
/// change that makes its handler treat `-` explicitly (bounded, UTF-8 checked,
/// with the same diagnostic escaping as any other input). The list ships
/// empty because no slice reads stdin yet; the cloudwatch `--json` entry is
/// added when that slice is integrated.
pub const STDIO_DASH: &[(&str, &str)] = &[];

/// Whether `-` is an accepted stdio marker for `flag` (a value flag name, or
/// [`STDIO_DASH_POSITIONAL`]) of `spec` according to `allowed`.
fn dash_listed(allowed: &[(&str, &str)], spec: &CommandSpec, flag: &str) -> bool {
    let path = spec.path.join(" ");
    allowed
        .iter()
        .any(|(command, listed)| *command == path && *listed == flag)
}

/// Whether `value` may follow the value flag `flag` of `spec` (see
/// [`CommandSpec::strict_values`]). The exact token `-` additionally needs
/// the pair to be listed in `allowed` ([`STDIO_DASH`]).
fn value_allowed(spec: &CommandSpec, allowed: &[(&str, &str)], flag: &str, value: &str) -> bool {
    !spec.strict_values
        || !(value.is_empty() || value.starts_with('-'))
        || (value == "-" && dash_listed(allowed, spec, flag))
}

/// Whether `name` is a value flag of `spec`, plain or repeatable.
fn is_value_flag(spec: &CommandSpec, name: &str) -> bool {
    spec.value_flags.contains(&name) || spec.repeatable_flags.contains(&name)
}

/// Routes already-decoded arguments (without the program name).
///
/// `help`/`-h`/`--help` and `version`/`-V`/`--version` as the first argument
/// win over everything after them. Inside a command, `-h`/`--help` selects
/// help, any other `-`-prefixed word must be one of the command's flags or
/// value flags and there may be at most `max_positionals` other words. A value
/// flag takes the next word (or the text after `=`) as its value; repeating it
/// keeps the last value (every value, in order, for a flag in
/// [`CommandSpec::repeatable_flags`]), and a value flag at the end of the
/// arguments with no value is a usage error.
///
/// # Errors
///
/// Returns a usage error (exit status 2) for no arguments, an unknown
/// command, an unknown flag or too many positional arguments.
pub fn route<'a>(specs: &'a [CommandSpec], args: &[String]) -> Result<Routed<'a>, ToolkitError> {
    route_with(specs, STDIO_DASH, args)
}

/// [`route`] with an explicit [`STDIO_DASH`] allowlist, so tests can exercise
/// the opt-in while the shipped constant is empty.
fn route_with<'a>(
    specs: &'a [CommandSpec],
    stdio_dash: &[(&str, &str)],
    args: &[String],
) -> Result<Routed<'a>, ToolkitError> {
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
        // A bare `-` is a positional only where STDIO_DASH lists the command's
        // `<positional>`; otherwise it is an unknown flag, as before #4743.
        if word.starts_with('-')
            && !(word == "-" && dash_listed(stdio_dash, spec, STDIO_DASH_POSITIONAL))
        {
            if is_value_flag(spec, word) {
                // `--name value`: the next word is the value.
                let Some(value) = rest.next() else {
                    return Err(usage_error());
                };
                if !value_allowed(spec, stdio_dash, word, value) {
                    return Err(usage_error());
                }
                invocation.store_value(spec, word, value);
            } else if let Some((name, value)) = word
                .split_once('=')
                .filter(|(name, _)| is_value_flag(spec, name))
            {
                if !value_allowed(spec, stdio_dash, name, value) {
                    return Err(usage_error());
                }
                invocation.store_value(spec, name, value);
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

/// The command-line values (and the value half of `--flag=value`) that contain a
/// newline: the only argument text that can forge a line in a multi-line message.
fn untrusted_values(args: &[String]) -> Vec<String> {
    let mut values = Vec::new();
    for arg in args {
        if arg.contains('\n') {
            values.push(arg.clone());
            if let Some((_, value)) = arg.split_once('=') {
                if value.contains('\n') {
                    values.push(value.to_string());
                }
            }
        }
    }
    values
}

/// Writes the error report and returns the exit status.
fn report(err: &mut dyn Write, error: &ToolkitError, untrusted: &[String]) -> u8 {
    let message = neutralize_text_knowing(error.message(), untrusted);
    let text = match error.kind() {
        ErrorKind::Usage => format!("error: {message}\n\n{USAGE}\n"),
        ErrorKind::Runtime => format!("error: {message}\n"),
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
        Err(error) => report(err, &error, &[]),
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
                &[],
            );
        };
        text_args.push(text.to_string());
    }
    let untrusted = untrusted_values(&text_args);
    match route(COMMANDS, &text_args) {
        Ok(Routed::Help) => print_static(out, err, USAGE),
        Ok(Routed::Version) => {
            print_static(out, err, &format!("toolkit {}", env!("CARGO_PKG_VERSION")))
        }
        Ok(Routed::Run(spec, invocation)) => {
            let mut output = Output::with_diagnostics(out, err);
            output.set_untrusted(&text_args);
            let result = (spec.handler)(&invocation, &mut output).and_then(|()| output.finish());
            match result {
                Ok(()) => output.exit_status(),
                Err(error) => report(err, &error, &untrusted),
            }
        }
        Err(error) => report(err, &error, &untrusted),
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

    /// Two commands with the same flags: `strict` rejects empty and dash values,
    /// `loose` takes the next word whatever it looks like.
    const REPEATABLE_SPECS: &[CommandSpec] = &[
        CommandSpec {
            path: &["strict"],
            flags: &["--json"],
            value_flags: &["--mode"],
            strict_values: true,
            repeatable_flags: &["--workload"],
            max_positionals: 1,
            passthrough: false,
            handler: skills_list,
        },
        CommandSpec {
            path: &["loose"],
            flags: &["--json"],
            value_flags: &["--mode", "--both"],
            strict_values: false,
            repeatable_flags: &["--workload", "--both"],
            max_positionals: 1,
            passthrough: false,
            handler: skills_list,
        },
    ];

    fn route_repeatable(values: &[&str]) -> Result<Invocation, ToolkitError> {
        match route(REPEATABLE_SPECS, &words(values))? {
            Routed::Run(_, invocation) => Ok(invocation),
            other => panic!("expected a runnable route, got {other:?}"),
        }
    }

    fn workloads(values: &[&str]) -> Vec<String> {
        let invocation = route_repeatable(values).expect("routes");
        invocation
            .values_of("--workload")
            .map(String::from)
            .collect()
    }

    #[test]
    fn a_repeatable_flag_keeps_every_value_in_order_in_either_spelling() {
        assert_eq!(
            workloads(&[
                "strict",
                "--workload",
                "a",
                "--workload=b",
                "--workload",
                "c"
            ]),
            ["a", "b", "c"]
        );
        // The same value twice is two values, not a collapse.
        assert_eq!(
            workloads(&["strict", "--workload=a", "--workload", "a"]),
            ["a", "a"]
        );
        // `value` is still the last one.
        let invocation =
            route_repeatable(&["strict", "--workload", "a", "--workload", "b"]).expect("routes");
        assert_eq!(invocation.value("--workload"), Some("b"));
    }

    #[test]
    fn a_single_or_absent_repeatable_flag_yields_one_or_no_value() {
        assert_eq!(workloads(&["strict", "--workload", "only"]), ["only"]);
        assert_eq!(workloads(&["strict", "--workload=only"]), ["only"]);
        let invocation = route_repeatable(&["strict", "--json"]).expect("routes");
        assert_eq!(invocation.values_of("--workload").count(), 0);
        assert_eq!(invocation.value("--workload"), None);
        assert_eq!(Invocation::default().values_of("--workload").count(), 0);
    }

    #[test]
    fn repeated_values_interleave_with_other_flags_and_positionals() {
        let invocation = route_repeatable(&[
            "strict",
            "--workload",
            "a",
            "--mode",
            "m1",
            "pos",
            "--json",
            "--workload=b",
            "--mode=m2",
            "--json",
            "--workload",
            "c",
        ])
        .expect("routes");
        assert_eq!(
            invocation.values_of("--workload").collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        // A plain value flag between them still keeps only its last value.
        assert_eq!(invocation.values_of("--mode").collect::<Vec<_>>(), ["m2"]);
        assert_eq!(invocation.value("--mode"), Some("m2"));
        assert_eq!(invocation.flags, vec!["--json".to_string()]);
        assert_eq!(invocation.positionals, vec!["pos".to_string()]);
        // Entries of the repeatable flag keep their order relative to each other.
        let order: Vec<_> = invocation
            .values
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            order,
            [
                ("--workload", "a"),
                ("--mode", "m2"),
                ("--workload", "b"),
                ("--workload", "c"),
            ]
        );
    }

    /// Routes `values` against `REPEATABLE_SPECS` with an explicit `-` allowlist.
    fn route_dash(allowed: &[(&str, &str)], values: &[&str]) -> Result<Invocation, ToolkitError> {
        match route_with(REPEATABLE_SPECS, allowed, &words(values))? {
            Routed::Run(_, invocation) => Ok(invocation),
            other => panic!("expected a runnable route, got {other:?}"),
        }
    }

    #[test]
    fn the_shipped_stdio_dash_list_is_empty_until_a_slice_reads_stdin() {
        assert!(STDIO_DASH.is_empty(), "{STDIO_DASH:?}");
    }

    #[test]
    fn an_unlisted_bare_dash_is_refused_as_before_4743() {
        // Strict command: value and positional `-` are refused, in both spellings.
        for values in [
            ["strict", "--mode", "-"].as_slice(),
            &["strict", "--mode=-"],
            &["strict", "--workload", "-"],
            &["strict", "--workload=-"],
            &["strict", "-"],
            &["strict", "--mode", "m", "-", "-"],
        ] {
            let Err(error) = route_repeatable(values) else {
                panic!("{values:?} must be a usage error");
            };
            assert_eq!(error.kind(), ErrorKind::Usage, "{values:?}");
            assert_eq!(error.code(), "invalid_arguments", "{values:?}");
            assert_eq!(error.message(), "invalid arguments", "{values:?}");
        }
        // A listed pair does not open its neighbours: another flag on the same
        // command, the same flag on another command, or the positional.
        let allowed = [("strict", "--mode")];
        assert!(route_dash(&allowed, &["strict", "--workload", "-"]).is_err());
        assert!(route_dash(&allowed, &["strict", "--workload=-"]).is_err());
        assert!(route_dash(&allowed, &["strict", "-"]).is_err());
        let other_command = [("loose", "<positional>")];
        assert!(route_dash(&other_command, &["strict", "-"]).is_err());
        let other_flag = [("loose", "--workload")];
        assert!(route_dash(&other_flag, &["strict", "--workload", "-"]).is_err());
        // Listing the positional does not allow `-` as a flag value.
        let positional_only = [("strict", STDIO_DASH_POSITIONAL)];
        assert!(route_dash(&positional_only, &["strict", "--mode", "-"]).is_err());
        assert!(route_dash(&positional_only, &["strict", "--mode=-"]).is_err());
    }

    #[test]
    fn a_non_strict_command_keeps_taking_any_value_unlisted() {
        // Unchanged by #4743: only `strict_values` commands ever refused values.
        let invocation = route_repeatable(&["loose", "--mode", "-"]).expect("routes");
        assert_eq!(invocation.value("--mode"), Some("-"));
        // But a positional `-` was always an unknown flag, listed or not.
        assert!(route_repeatable(&["loose", "-"]).is_err());
    }

    #[test]
    fn a_listed_pair_accepts_a_bare_dash_value_in_both_spellings() {
        let allowed = [("strict", "--mode"), ("strict", "--workload")];
        let invocation = route_dash(&allowed, &["strict", "--mode", "-"]).expect("routes");
        assert_eq!(invocation.value("--mode"), Some("-"));
        let invocation = route_dash(&allowed, &["strict", "--mode=-"]).expect("routes");
        assert_eq!(invocation.value("--mode"), Some("-"));
        let invocation = route_dash(
            &allowed,
            &[
                "strict",
                "--workload",
                "-",
                "--workload=-",
                "--workload",
                "a",
            ],
        )
        .expect("routes");
        assert_eq!(
            invocation.values_of("--workload").collect::<Vec<_>>(),
            ["-", "-", "a"]
        );
    }

    #[test]
    fn a_listed_pair_still_refuses_every_other_dashed_or_empty_value() {
        let allowed = [("strict", "--mode"), ("strict", "--workload")];
        for bad in ["-x", "--foo", "--", "-=", "-- ", "-\u{0}", "--json", ""] {
            for flag in ["--mode", "--workload"] {
                assert!(
                    route_dash(&allowed, &["strict", flag, bad]).is_err(),
                    "{flag} {bad:?}"
                );
                let attached = format!("{flag}={bad}");
                assert!(
                    route_dash(&allowed, &["strict", &attached]).is_err(),
                    "{attached:?}"
                );
            }
        }
        // `--mode -` does not swallow the next flag as a value either.
        assert!(route_dash(&allowed, &["strict", "--mode", "-", "--bogus"]).is_err());
    }

    #[test]
    fn a_listed_positional_accepts_a_bare_dash_and_counts_it() {
        let allowed = [("strict", STDIO_DASH_POSITIONAL)];
        let invocation = route_dash(&allowed, &["strict", "-"]).expect("routes");
        assert_eq!(invocation.positionals, ["-"]);
        // Beside a switch and a value flag, in any order.
        let invocation =
            route_dash(&allowed, &["strict", "--json", "--mode", "m", "-"]).expect("routes");
        assert_eq!(invocation.positionals, ["-"]);
        assert!(invocation.has_flag("--json"));
        assert_eq!(invocation.value("--mode"), Some("m"));
        // It counts against `max_positionals`.
        assert!(route_dash(&allowed, &["strict", "-", "-"]).is_err());
        assert!(route_dash(&allowed, &["strict", "a", "-"]).is_err());
        // Other dash-prefixed words are still unknown flags.
        assert!(route_dash(&allowed, &["strict", "-x"]).is_err());
        assert!(route_dash(&allowed, &["strict", "--"]).is_err());
        // A command that takes no positionals refuses it even when listed.
        let none = &[CommandSpec {
            path: &["none"],
            flags: &[],
            value_flags: &[],
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: skills_list,
        }];
        let listed = [("none", STDIO_DASH_POSITIONAL)];
        assert!(route_with(none, &listed, &words(&["none", "-"])).is_err());
    }

    #[test]
    fn a_listed_multi_word_command_path_is_matched_space_joined() {
        let specs = &[CommandSpec {
            path: &["group", "leaf"],
            flags: &[],
            value_flags: &["--json"],
            strict_values: true,
            repeatable_flags: &[],
            max_positionals: 0,
            passthrough: false,
            handler: skills_list,
        }];
        let args = words(&["group", "leaf", "--json", "-"]);
        assert!(route_with(specs, &[], &args).is_err());
        assert!(route_with(specs, &[("group", "--json")], &args).is_err());
        assert!(route_with(specs, &[("group leaf", "--json")], &args).is_ok());
    }

    #[test]
    fn strict_values_refuse_each_repeated_value_that_is_empty_or_dashed() {
        for bad in [
            ["--workload", ""].as_slice(),
            &["--workload", "-x"],
            &["--workload", "--json"],
            &["--workload="],
            &["--workload=-x"],
            &["--workload"],
        ] {
            for position in [0, 1] {
                let mut values = vec!["strict"];
                if position == 1 {
                    values.extend(["--workload", "ok"]);
                }
                values.extend_from_slice(bad);
                if position == 0 {
                    values.extend(["--workload", "ok"]);
                }
                let Err(error) = route_repeatable(&values) else {
                    panic!("{values:?} must be a usage error");
                };
                assert_eq!(error.kind(), ErrorKind::Usage, "{values:?}");
                assert_eq!(error.exit_code(), 2);
            }
        }
    }

    #[test]
    fn a_non_strict_command_takes_any_next_word_for_every_repeated_value() {
        let invocation = route_repeatable(&[
            "loose",
            "--workload",
            "--json",
            "--workload=",
            "--workload",
            "",
            "--workload=-x",
        ])
        .expect("routes");
        assert_eq!(
            invocation.values_of("--workload").collect::<Vec<_>>(),
            ["--json", "", "", "-x"]
        );
        assert!(invocation.flags.is_empty());
    }

    #[test]
    fn a_plain_value_flag_still_keeps_only_its_last_value_when_repeated() {
        let invocation = route_repeatable(&["strict", "--mode", "a", "--mode=b", "--mode", "c"])
            .expect("routes");
        assert_eq!(invocation.values_of("--mode").collect::<Vec<_>>(), ["c"]);
        assert_eq!(invocation.value("--mode"), Some("c"));
        assert_eq!(invocation.values.len(), 1);
        // A flag in both lists is repeatable.
        let invocation = route_repeatable(&["loose", "--both", "a", "--both=b"]).expect("routes");
        assert_eq!(
            invocation.values_of("--both").collect::<Vec<_>>(),
            ["a", "b"]
        );
        // Repeatable flags are only known to the commands that declare them, and
        // `--help` still wins.
        assert!(route(COMMANDS, &words(&["sync", "vendored", "--workload", "a"])).is_err());
        assert!(matches!(
            route(
                REPEATABLE_SPECS,
                &words(&["strict", "--workload", "a", "--help"])
            ),
            Ok(Routed::Help)
        ));
    }

    #[test]
    fn registered_repeatable_flags_are_not_also_plain_flags() {
        for spec in COMMANDS {
            for flag in spec.repeatable_flags {
                assert!(
                    !spec.flags.contains(flag),
                    "{:?}: {flag} is both a switch and a repeatable value flag",
                    spec.path
                );
            }
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
            repeatable_flags: &[],
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
