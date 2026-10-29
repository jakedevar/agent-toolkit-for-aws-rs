//! Per-slice command registration (#4576).
//!
//! Every slice (a command family and its handlers) owns its registration: a
//! [`Slice`] constant, usually named `SLICE`, defined in the slice's own module
//! next to its handlers. It carries the slice's [`CommandSpec`] routes and its
//! help text, so adding a slice never edits a table, a help block or a README
//! list that other slices also edit.
//!
//! [`SLICES`] is the one shared list. It holds one line per slice and is kept
//! in strictly ascending order of the slice name, so two slices added
//! concurrently touch different lines unless they land in the same alphabetical
//! gap, where the conflict is trivial: keep both lines in sorted order (a test
//! fails if the list is unsorted, holds a duplicate name or a duplicate route).
//!
//! [`COMMANDS`] and [`USAGE`] are built from [`SLICES`] at compile time:
//!
//! - [`COMMANDS`] concatenates the routes of every slice in the legacy routing
//!   order ([`LEGACY_ROUTING_ORDER`]: the order of the pre-registry table, which
//!   is independent of the help order); a slice that is not listed there follows
//!   the listed ones, ordered by name. Inside a slice the routes keep their
//!   declared order, which matters because the first matching route wins
//!   (`aurora acu` follows the longer `aurora acu ...` routes).
//! - [`USAGE`] is [`USAGE_HEAD`], the synopses ordered by
//!   [`Slice::synopsis_rank`], [`USAGE_MIDDLE`], the descriptions ordered by
//!   [`Slice::description_rank`], [`USAGE_TAIL`] and the exit-code notes ordered
//!   like the descriptions.
//!
//! The two ranks only exist to keep the help of the slices that predate this
//! registry byte-identical to what it was (their synopsis and description
//! orders differ in places). A new slice sets both ranks to [`NEW_RANK`]: its
//! help follows the earlier slices, ordered by name, and no number has to be
//! allocated or shared between concurrent slices.
//!
//! The README no longer carries a hand-maintained command list. `toolkit help`
//! is the list, and the test `every_slice_documents_its_commands` fails when a
//! slice's [`Slice::doc`] file stops mentioning one of its commands.

use crate::CommandSpec;

/// The rank of a slice added after the registry existed: after every earlier
/// slice, then ordered by name.
pub(crate) const NEW_RANK: u16 = 1000;

/// What one slice contributes to the command table and the help text.
#[derive(Clone, Copy)]
pub(crate) struct Slice {
    /// Unique name, also the sort key of [`SLICES`] (lowercase, `-` separated).
    pub name: &'static str,
    /// The routes of the slice, in routing order (first match wins).
    pub commands: &'static [CommandSpec],
    /// Lines for the `Usage:` block: each line starts with two spaces and
    /// `toolkit` and ends with a newline. Continuation lines are indented
    /// further.
    pub synopsis: &'static str,
    /// Blocks for the `Commands:` section: each line starts with two spaces
    /// (a block head) or more (a continuation) and ends with a newline.
    pub description: &'static str,
    /// Lines appended to the `Exit codes:` section (each ends with a newline),
    /// or the empty string when the shared text already covers the slice.
    pub exit_notes: &'static str,
    /// Repository-relative path of the document that describes the commands.
    /// Every command path must appear in it (see the module docs).
    #[allow(dead_code)] // read by the documentation test only
    pub doc: &'static str,
    /// Position of the synopsis lines in `Usage:` (see [`NEW_RANK`]).
    pub synopsis_rank: u16,
    /// Position of the description (and exit notes) in `Commands:` (see
    /// [`NEW_RANK`]). The routes of [`COMMANDS`] have their own order.
    pub description_rank: u16,
}

/// Every slice, one per line, ascending by [`Slice::name`]. Add a slice by
/// inserting one line at its alphabetical place.
pub(crate) const SLICES: &[Slice] = &[
    crate::ai_data::SLICE,
    crate::ai_models::SLICE,
    crate::ai_payloads::SLICE,
    crate::asm_exec::SLICE,
    crate::aurora::SLICE,
    crate::bedrock_agent::SLICE,
    crate::bedrock_probes::SLICE,
    crate::bedrock_report::SLICE,
    crate::bedrock_resolve::SLICE,
    crate::cloudwatch::API_SLICE,
    crate::cloudwatch::DOMAIN_SLICE,
    crate::docgraph::SLICE,
    crate::dynamodb::SLICE,
    crate::dynamodb_bench::SLICE,
    crate::dynamodb_deploy::SLICE,
    crate::elasticache::SLICE,
    crate::elasticache::tunnel::SLICE,
    crate::launch::SLICE,
    crate::manifests::SLICE,
    crate::marketplace::SLICE,
    crate::mcp_proxy::command::SLICE,
    crate::msk::SLICE,
    crate::relational::SLICE,
    crate::skills::SLICE,
    crate::spec::SLICE,
    crate::startup::artifacts::SLICE,
    crate::startup::consent::SLICE,
    crate::startup::diagram::SLICE,
    crate::startup::fixtures::SLICE,
    crate::startup::model_ids::SLICE,
    crate::startup::model_path::SLICE,
    crate::startup::model_staleness::SLICE,
    crate::startup::models::SLICE,
    crate::startup::parity::SLICE,
    crate::startup::plan::SLICE,
    crate::startup::pricing::SLICE,
    crate::startup::program::SLICE,
    crate::startup::report::SLICE,
    crate::startup::scoring::SLICE,
    crate::startup::skill_refs::SLICE,
    crate::startup::terraform::SLICE,
    crate::sync::SLICE,
    crate::telemetry::SLICE,
];

/// The slices that predate the registry, in the order their routes had in the
/// old `COMMANDS` table. It fixes the public order of [`COMMANDS`] (the test
/// `the_public_route_order_is_pinned` pins every route). A new slice is not
/// listed here: its routes follow these, ordered by slice name.
const LEGACY_ROUTING_ORDER: &[&str] = &[
    "skills",
    "manifests",
    "spec",
    "startup-pricing",
    "startup-program",
    "startup-model-ids",
    "startup-artifacts",
    "startup-plan",
    "startup-consent",
    "startup-parity",
    "startup-terraform",
    "startup-report",
    "startup-scoring",
    "startup-models",
    "startup-diagram",
    "startup-model-path",
    "startup-skill-refs",
    "startup-model-staleness",
    "startup-fixtures",
    "sync",
    "msk",
    "dynamodb",
    "dynamodb-deploy",
    "aurora",
    "ai-data",
    "ai-models",
    "bedrock-agent",
    "bedrock-report",
    "cloudwatch-domain",
    "cloudwatch-api",
    "elasticache",
    "bedrock-resolve",
    "asm-exec",
    "relational",
];

/// The start of the usage text, up to the first synopsis line.
const USAGE_HEAD: &str = "Usage:\n";

/// Between the last synopsis line and the first command description.
const USAGE_MIDDLE: &str = "  toolkit --version\n  toolkit help\n\nCommands:\n";

/// After the last description: the shared option and exit-code text. The
/// exit-code notes of the slices follow it; the text ends in a newline that
/// [`USAGE`] drops.
const USAGE_TAIL: &str = "
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
";

const SLICE_COUNT: usize = SLICES.len();

/// Which order [`slice_order`] computes.
#[derive(Clone, Copy)]
enum Order {
    Synopsis,
    Description,
    Routing,
}

/// Whether two names are equal (`==` on `str` is not `const`).
const fn same_name(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    let mut at = 0;
    while at < left.len() {
        if left[at] != right[at] {
            return false;
        }
        at += 1;
    }
    true
}

/// The position of a slice in [`LEGACY_ROUTING_ORDER`], or past its end.
const fn routing_position(slice: &Slice) -> usize {
    let mut at = 0;
    while at < LEGACY_ROUTING_ORDER.len() {
        if same_name(LEGACY_ROUTING_ORDER[at], slice.name) {
            return at;
        }
        at += 1;
    }
    LEGACY_ROUTING_ORDER.len()
}

/// Whether `a` sorts before `b` by the chosen key and then the name.
const fn comes_first(left: &Slice, right: &Slice, order: Order) -> bool {
    let (rank_left, rank_right) = match order {
        Order::Synopsis => (left.synopsis_rank as usize, right.synopsis_rank as usize),
        Order::Description => (
            left.description_rank as usize,
            right.description_rank as usize,
        ),
        Order::Routing => (routing_position(left), routing_position(right)),
    };
    if rank_left != rank_right {
        return rank_left < rank_right;
    }
    let (name_left, name_right) = (left.name.as_bytes(), right.name.as_bytes());
    let mut at = 0;
    while at < name_left.len() && at < name_right.len() {
        if name_left[at] != name_right[at] {
            return name_left[at] < name_right[at];
        }
        at += 1;
    }
    name_left.len() < name_right.len()
}

/// The indices of [`SLICES`] in the given order (an insertion sort: the list is
/// short and `const` evaluation has no `sort`).
const fn slice_order(kind: Order) -> [usize; SLICE_COUNT] {
    let mut order = [0usize; SLICE_COUNT];
    let mut i = 0;
    while i < SLICE_COUNT {
        let mut at = i;
        while at > 0 && comes_first(&SLICES[i], &SLICES[order[at - 1]], kind) {
            order[at] = order[at - 1];
            at -= 1;
        }
        order[at] = i;
        i += 1;
    }
    order
}

const SYNOPSIS_ORDER: [usize; SLICE_COUNT] = slice_order(Order::Synopsis);
const DESCRIPTION_ORDER: [usize; SLICE_COUNT] = slice_order(Order::Description);
const ROUTING_ORDER: [usize; SLICE_COUNT] = slice_order(Order::Routing);

const COMMAND_COUNT: usize = {
    let mut count = 0;
    let mut i = 0;
    while i < SLICE_COUNT {
        count += SLICES[i].commands.len();
        i += 1;
    }
    count
};

const fn command_table() -> [CommandSpec; COMMAND_COUNT] {
    // Any real route is a fine filler: every cell is overwritten below.
    let mut table = [SLICES[0].commands[0]; COMMAND_COUNT];
    let mut at = 0;
    let mut i = 0;
    while i < SLICE_COUNT {
        let commands = SLICES[ROUTING_ORDER[i]].commands;
        let mut j = 0;
        while j < commands.len() {
            table[at] = commands[j];
            at += 1;
            j += 1;
        }
        i += 1;
    }
    table
}

const COMMAND_TABLE: [CommandSpec; COMMAND_COUNT] = command_table();

/// The routing table: the routes of every slice in [`SLICES`], see the module
/// docs for the order.
pub const COMMANDS: &[CommandSpec] = &COMMAND_TABLE;

/// The fragments of the usage text in output order.
const fn usage_part(index: usize) -> &'static str {
    let synopses = SLICE_COUNT;
    if index == 0 {
        USAGE_HEAD
    } else if index <= synopses {
        SLICES[SYNOPSIS_ORDER[index - 1]].synopsis
    } else if index == synopses + 1 {
        USAGE_MIDDLE
    } else if index <= 2 * synopses + 1 {
        SLICES[DESCRIPTION_ORDER[index - synopses - 2]].description
    } else if index == 2 * synopses + 2 {
        USAGE_TAIL
    } else {
        SLICES[DESCRIPTION_ORDER[index - 2 * synopses - 3]].exit_notes
    }
}

/// Number of fragments of the usage text.
const USAGE_PARTS: usize = 3 * SLICE_COUNT + 3;

const USAGE_LEN: usize = {
    let mut len = 0;
    let mut i = 0;
    while i < USAGE_PARTS {
        len += usage_part(i).len();
        i += 1;
    }
    len
};

#[allow(
    clippy::large_stack_arrays,
    reason = "evaluated at compile time only; the array is the usage text"
)]
const fn usage_bytes() -> [u8; USAGE_LEN] {
    let mut bytes = [0u8; USAGE_LEN];
    let mut at = 0;
    let mut i = 0;
    while i < USAGE_PARTS {
        let part = usage_part(i).as_bytes();
        let mut j = 0;
        while j < part.len() {
            bytes[at] = part[j];
            at += 1;
            j += 1;
        }
        i += 1;
    }
    bytes
}

#[allow(
    clippy::large_const_arrays,
    reason = "a static cannot feed a const (rustc ICE); it is only read at compile time"
)]
const USAGE_BYTES: [u8; USAGE_LEN] = usage_bytes();

const fn usage_text() -> &'static str {
    // Drop the final newline: the usage text has none.
    let (text, _) = USAGE_BYTES.split_at(USAGE_LEN - 1);
    match core::str::from_utf8(text) {
        Ok(text) => text,
        Err(_) => panic!("the usage fragments are not UTF-8"),
    }
}

/// Usage text printed by `help` (stdout) and after usage errors (stderr),
/// assembled from the shared text and the help of every slice in [`SLICES`].
pub const USAGE: &str = usage_text();

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashSet};
    use std::fs;
    use std::path::Path;

    #[test]
    fn slices_are_listed_one_per_line_in_ascending_name_order() {
        let names: Vec<&str> = SLICES.iter().map(|slice| slice.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            names, sorted,
            "SLICES must be strictly ascending by name (no duplicates)"
        );
    }

    #[test]
    fn every_route_is_registered_once_and_none_is_shadowed() {
        let mut seen = HashSet::new();
        for (index, spec) in COMMANDS.iter().enumerate() {
            assert!(seen.insert(spec.path), "duplicate route {:?}", spec.path);
            for earlier in &COMMANDS[..index] {
                assert!(
                    !spec.path.starts_with(earlier.path),
                    "route {:?} is shadowed by the earlier route {:?}",
                    spec.path,
                    earlier.path
                );
            }
        }
        let declared: usize = SLICES.iter().map(|slice| slice.commands.len()).sum();
        assert_eq!(COMMANDS.len(), declared, "a route went missing");
        assert!(SLICES.iter().all(|slice| !slice.commands.is_empty()));
    }

    #[test]
    fn a_slice_keeps_its_declared_route_order() {
        for slice in SLICES {
            let positions: Vec<usize> = slice
                .commands
                .iter()
                .map(|spec| {
                    COMMANDS
                        .iter()
                        .position(|route| route.path == spec.path)
                        .expect("registered")
                })
                .collect();
            assert!(
                positions.windows(2).all(|pair| pair[0] < pair[1]),
                "slice {} lost its routing order",
                slice.name
            );
        }
    }

    #[test]
    fn every_slice_documents_its_routes_in_its_own_help() {
        for slice in SLICES {
            assert!(
                slice.synopsis.ends_with('\n') && slice.description.ends_with('\n'),
                "slice {}: help fragments end with a newline",
                slice.name
            );
            assert!(
                slice.exit_notes.is_empty() || slice.exit_notes.ends_with('\n'),
                "slice {}: exit notes end with a newline",
                slice.name
            );
            for line in slice.synopsis.lines() {
                assert!(
                    line.starts_with("  toolkit ") || line.starts_with("      "),
                    "slice {}: synopsis line {line:?}",
                    slice.name
                );
            }
            for line in slice.description.lines() {
                assert!(
                    line.starts_with("  "),
                    "slice {}: description line {line:?}",
                    slice.name
                );
            }
            for spec in slice.commands {
                let line = format!("  toolkit {}", spec.path.join(" "));
                assert!(
                    slice.synopsis.contains(&line),
                    "slice {}: the synopsis lacks '{line}'",
                    slice.name
                );
            }
        }
    }

    #[test]
    fn the_usage_text_is_the_ordered_fragments() {
        assert!(USAGE.starts_with("Usage:\n  toolkit "));
        assert!(USAGE.ends_with('.'), "no trailing newline");
        for slice in SLICES {
            assert!(USAGE.contains(slice.synopsis), "{}", slice.name);
            assert!(USAGE.contains(slice.description), "{}", slice.name);
        }
        let usage_end = USAGE.find("\nOptions:\n").expect("options");
        let commands = USAGE.find("\nCommands:\n").expect("commands");
        assert!(commands < usage_end);
        // The synopsis rank and the description rank order the fragments.
        let mut by_synopsis: Vec<&Slice> = SLICES.iter().collect();
        by_synopsis.sort_by_key(|slice| (slice.synopsis_rank, slice.name));
        let mut at = 0;
        for slice in by_synopsis {
            let found = USAGE[at..].find(slice.synopsis).expect("synopsis in order");
            at += found + slice.synopsis.len();
        }
        let mut by_description: Vec<&Slice> = SLICES.iter().collect();
        by_description.sort_by_key(|slice| (slice.description_rank, slice.name));
        let mut at = commands;
        for slice in by_description {
            let found = USAGE[at..]
                .find(slice.description)
                .expect("description in order");
            at += found + slice.description.len();
        }
    }

    #[test]
    fn every_slice_documents_its_commands() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut problems = Vec::new();
        for slice in SLICES {
            let Ok(text) = fs::read_to_string(root.join(slice.doc)) else {
                problems.push(format!("{}: cannot read its doc {}", slice.name, slice.doc));
                continue;
            };
            for spec in slice.commands {
                let command = spec.path.join(" ");
                if !text.contains(&command) {
                    problems.push(format!(
                        "{}: {} does not mention `{command}`",
                        slice.name, slice.doc
                    ));
                }
            }
        }
        assert!(
            problems.is_empty(),
            "document each command in its slice's `Slice::doc` file (or point `doc` at the file \
             that does):\n{}",
            problems.join("\n")
        );
    }

    /// The route paths of the old `COMMANDS` table (commit ce0eaf6a, before the
    /// registry), in table order: the public order of [`COMMANDS`]. The routes of
    /// slices added after the registry follow at the end, ordered by slice name.
    const LEGACY_COMMAND_ORDER: [&str; 104] = [
        "skills list",
        "manifests validate",
        "spec validate",
        "startup model-pricing",
        "startup program-artifacts",
        "startup model-id-lint",
        "startup artifacts",
        "startup emit-plan",
        "startup consent-audit",
        "startup pricing-parity",
        "startup terraform-policy",
        "startup migration-report",
        "startup heroku-report",
        "startup score",
        "startup score-units",
        "startup model-recommend",
        "startup build-diagram",
        "startup verify-model-path",
        "startup skill-refs",
        "startup model-staleness",
        "startup check-expected",
        "sync plugin-skills",
        "sync vendored",
        "msk size",
        "msk compat",
        "msk sim compute",
        "msk sim render",
        "msk sim validate",
        "dynamodb calculate-costs",
        "dynamodb iterate-design",
        "dynamodb deploy-model",
        "dynamodb generate-teardown",
        "aurora acu estimate",
        "aurora acu list-instances",
        "aurora acu pricing-source",
        "aurora acu",
        "aurora commitment",
        "aurora io",
        "ai-data detect-format",
        "ai-data validate-metrics",
        "ai-data transform plan",
        "ai-data transform submit",
        "ai-data transform describe",
        "ai-models list-models",
        "ai-models filter",
        "ai-models search",
        "ai-models model-names",
        "ai-models recipes",
        "ai-models endpoint-diagnostics",
        "bedrock-agent fetch",
        "bedrock-report render",
        "bedrock-report validate",
        "cloudwatch render",
        "cloudwatch validate",
        "cloudwatch region",
        "cloudwatch instrumentation",
        "cloudwatch snapshots",
        "cloudwatch evaluate-traces",
        "cloudwatch capture-dataset",
        "elasticache price",
        "elasticache estimate-serverless",
        "elasticache validate-input",
        "elasticache validate-references",
        "elasticache preflight",
        "elasticache audit",
        "elasticache dashboard",
        "bedrock-resolve identity",
        "bedrock-resolve image",
        "bedrock-resolve pricing",
        "bedrock-resolve source-model",
        "asm exec",
        "dsql selftest",
        "dsql tenant-sql",
        "dsql tenant-query",
        "rds commitment",
        "rds-oracle test-connection",
        "timestream-influxdb validate-input",
        "timestream-influxdb instance-types",
        // Slices added after the registry (NEW_RANK), by slice name.
        // ai-payloads (#4386, slice S1 #5143):
        "ai-payloads reward sample",
        "ai-payloads reward check",
        "ai-payloads templates",
        "ai-payloads render",
        // bedrock-probes (#4404):
        "bedrock-probes iam-policy",
        "bedrock-probes preflight",
        "bedrock-probes api-path",
        "bedrock-probes classify",
        "bedrock-probes source-baseline",
        // docgraph (#4395):
        "documentdb wa-review",
        "neptune validate-input",
        "neptune seed",
        "neptune memory",
        // dynamodb-bench (#4390):
        "dynamodb benchmark-model",
        "dynamodb generate-perf-report",
        // elasticache-tunnel (#4393):
        "elasticache find-tunnel-host",
        "elasticache start-tunnel",
        "elasticache test-connection",
        // launch callback and sign-in lifecycle (#5216):
        "launch auth-callback",
        "launch auth-start",
        "launch auth-wait",
        // marketplace (#4396):
        "marketplace handler",
        "marketplace query-metering",
        "marketplace test-events",
        // mcp-proxy (#4413 S3):
        "mcp-proxy",
        // telemetry (#4398):
        "telemetry",
    ];

    #[test]
    fn the_public_route_order_is_pinned() {
        let order: Vec<String> = COMMANDS.iter().map(|spec| spec.path.join(" ")).collect();
        assert!(order.len() >= LEGACY_COMMAND_ORDER.len());
        assert_eq!(
            order[..LEGACY_COMMAND_ORDER.len()],
            LEGACY_COMMAND_ORDER,
            "COMMANDS must keep the legacy routing order; routes of new slices follow it"
        );
    }

    #[test]
    fn the_legacy_routing_order_names_registered_slices_once() {
        let names: HashSet<&str> = SLICES.iter().map(|slice| slice.name).collect();
        let listed: HashSet<&str> = LEGACY_ROUTING_ORDER.iter().copied().collect();
        assert_eq!(
            listed.len(),
            LEGACY_ROUTING_ORDER.len(),
            "a name is listed twice"
        );
        assert!(listed.is_subset(&names), "an unknown slice is listed");
    }

    /// The first word after `toolkit` when `line` starts a command (a fenced
    /// or plain line, optionally behind a prompt, a list marker or a backtick).
    /// `toolkit help` and `toolkit --version` are not slice commands.
    fn command_word(line: &str) -> Option<String> {
        let mut text = line.trim_start();
        for marker in ["$ ", "- ", "* "] {
            text = text.strip_prefix(marker).unwrap_or(text).trim_start();
        }
        let text = text.trim_start_matches('`');
        let rest = text.strip_prefix("toolkit ")?.trim_start();
        let word: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        (word.starts_with(|c: char| c.is_ascii_lowercase()) && word != "help").then_some(word)
    }

    /// The command families (first path word) of every registered slice, one
    /// set per slice. A family can belong to several slices (`startup`,
    /// `cloudwatch`, `dynamodb`), and one slice can own several families (the
    /// relational slice owns `dsql`, `rds`, `rds-oracle` and
    /// `timestream-influxdb`).
    fn slice_families() -> Vec<HashSet<&'static str>> {
        SLICES
            .iter()
            .map(|slice| {
                slice
                    .commands
                    .iter()
                    .filter_map(|spec| spec.path.first().copied())
                    .collect()
            })
            .collect()
    }

    /// Why `readme` holds a hand-maintained command index, if it does.
    ///
    /// Allowed: the single `## Commands` pointer section (prose only: no code
    /// fence and no command line), and per-slice sections whose command lines
    /// (fenced or not, however much prose or blank lines separate them) all
    /// belong to ONE registered slice, see [`slice_families`]. Rejected: a
    /// second `Commands`/`Usage`-style heading, a fence or command line inside
    /// the pointer section, and a section whose commands name a family that no
    /// registered slice owns or families of two or more slices (an index across
    /// slices). Prose never authorizes a section: an introduction or heading
    /// that names the families changes nothing.
    fn command_index_problem(readme: &str) -> Option<String> {
        let slices = slice_families();
        let mut in_fence = false;
        let mut heading = String::new();
        let mut commands_headings = 0;
        let mut families: BTreeSet<String> = BTreeSet::new();
        let section_problem = |families: &mut BTreeSet<String>, heading: &str| {
            let found = std::mem::take(families);
            let one_slice = slices
                .iter()
                .any(|owned| found.iter().all(|family| owned.contains(family.as_str())));
            (!one_slice).then(|| {
                format!("commands of {found:?} under `{heading}` do not belong to one slice")
            })
        };
        for line in readme.lines() {
            let fence = line.trim_start().starts_with("```");
            if !in_fence && !fence {
                if let Some(text) = line.strip_prefix('#') {
                    if let Some(problem) = section_problem(&mut families, &heading) {
                        return Some(problem);
                    }
                    heading = text.trim_start_matches('#').trim().to_lowercase();
                    match heading.as_str() {
                        "commands" => commands_headings += 1,
                        "command list" | "commands list" | "command index" | "usage"
                        | "synopsis" => return Some(format!("a `{heading}` section")),
                        _ => {}
                    }
                    continue;
                }
            }
            let word = command_word(line);
            if heading == "commands" && (fence || word.is_some()) {
                return Some(format!(
                    "a fence or command line under `## Commands`: {line}"
                ));
            }
            families.extend(word);
            if fence {
                in_fence = !in_fence;
            }
        }
        if let Some(problem) = section_problem(&mut families, &heading) {
            return Some(problem);
        }
        (commands_headings != 1)
            .then(|| format!("{commands_headings} `Commands` headings (exactly one pointer)"))
    }

    fn readme() -> String {
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
            .expect("README.md")
    }

    #[test]
    fn the_readme_has_no_hand_maintained_command_list() {
        let readme = readme();
        assert_eq!(
            command_index_problem(&readme),
            None,
            "the command list is generated: `toolkit help` prints it from the slice registrations"
        );
        assert!(readme.contains("`toolkit help`"));
    }

    #[test]
    fn the_readme_guard_rejects_a_new_command_index() {
        let readme = readme();
        // A second Commands heading with a fenced list of two new slices.
        let table = format!(
            "{readme}\n## Commands\n\n```\ntoolkit analytics run INPUT\ntoolkit archive run INPUT\n```\n"
        );
        assert!(command_index_problem(&table).is_some());
        // The same list under another heading, blank lines between the rows.
        let index = format!(
            "{readme}\n## Everything\n\n```\ntoolkit analytics run INPUT\n\ntoolkit archive run INPUT\n```\n"
        );
        assert!(command_index_problem(&index).is_some());
        // Unfenced, behind a prompt, with a spacing change.
        let plain = format!(
            "{readme}\n## Index\n\n$ toolkit  analytics run INPUT\n- toolkit archive run\n"
        );
        assert!(command_index_problem(&plain).is_some());
        // The old table, put back into the pointer section.
        let old = readme.replacen(
            "## Commands\n",
            "## Commands\n\n```\ntoolkit skills list [ROOT] [--json]\ntoolkit sync  plugin-skills\n```\n",
            1,
        );
        assert!(command_index_problem(&old).is_some());
        // A Usage-style heading.
        assert!(command_index_problem(&format!("{readme}\n## Usage\n")).is_some());
    }

    #[test]
    fn the_readme_guard_rejects_prose_that_authorizes_a_cross_slice_index() {
        let readme = readme();
        // The review #4876 reproduction: the introduction lists every family,
        // then one fenced block holds all 78 routes.
        let families: Vec<&str> = LEGACY_COMMAND_ORDER
            .iter()
            .filter_map(|path| path.split(' ').next())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let routes = LEGACY_COMMAND_ORDER
            .iter()
            .map(|path| format!("toolkit {path}"))
            .collect::<Vec<_>>()
            .join("\n");
        let everything = format!(
            "{readme}\n## Toolkit reference\n\nAvailable families: {}.\n\n```\n{routes}\n```\n",
            families.join(", ")
        );
        assert!(command_index_problem(&everything).is_some());
        // A two-line block across two slices whose heading and introduction
        // name both families.
        let two = format!(
            "{readme}\n## msk and aurora\n\nExamples of `msk` and `aurora`.\n\n```\ntoolkit msk help\ntoolkit aurora acu\n```\n"
        );
        assert!(command_index_problem(&two).is_some());
        // The same two lines as a plain list, with no fence.
        let listed =
            format!("{readme}\n## msk and aurora\n\n- toolkit msk help\n- toolkit aurora acu\n");
        assert!(command_index_problem(&listed).is_some());
    }

    #[test]
    fn the_readme_guard_allows_per_slice_examples() {
        let readme = readme();
        // One family, one slice.
        let one =
            format!("{readme}\n## aurora\n\n```\ntoolkit aurora acu\ntoolkit aurora acu\n```\n");
        assert_eq!(command_index_problem(&one), None);
        // The relational slice owns four families, so its examples pass together
        // (without any prose that names them).
        let relational = format!(
            "{readme}\n## relational\n\n```\ntoolkit dsql selftest\ntoolkit rds commitment\ntoolkit rds-oracle test-connection\ntoolkit timestream-influxdb instance-types\n```\n"
        );
        assert_eq!(command_index_problem(&relational), None);
        // One family spread over two slices of that family.
        let startup = format!(
            "{readme}\n## startup\n\n```\ntoolkit startup artifacts\ntoolkit startup emit-plan\n```\n"
        );
        assert_eq!(command_index_problem(&startup), None);
    }
}
