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
//! concurrently touch different lines and merge without a conflict (a test
//! fails if the list is unsorted, holds a duplicate name or a duplicate route).
//!
//! [`COMMANDS`] and [`USAGE`] are built from [`SLICES`] at compile time:
//!
//! - [`COMMANDS`] concatenates the routes of every slice, ordered by
//!   [`Slice::description_rank`] and then the slice name. Inside a slice the
//!   routes keep their declared order, which matters because the first
//!   matching route wins (`aurora acu` follows the longer `aurora acu ...`
//!   routes).
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
#[allow(
    dead_code,
    reason = "the convention for slices added after the registry; no such slice exists yet"
)]
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
    /// Position of the description (and exit notes) in `Commands:` and of the
    /// routes in [`COMMANDS`] (see [`NEW_RANK`]).
    pub description_rank: u16,
}

/// Every slice, one per line, ascending by [`Slice::name`]. Add a slice by
/// inserting one line at its alphabetical place.
pub(crate) const SLICES: &[Slice] = &[
    crate::ai_data::SLICE,
    crate::ai_models::SLICE,
    crate::asm_exec::SLICE,
    crate::aurora::SLICE,
    crate::bedrock_agent::SLICE,
    crate::bedrock_report::SLICE,
    crate::bedrock_resolve::SLICE,
    crate::cloudwatch::API_SLICE,
    crate::cloudwatch::DOMAIN_SLICE,
    crate::dynamodb::SLICE,
    crate::dynamodb_deploy::SLICE,
    crate::elasticache::SLICE,
    crate::manifests::SLICE,
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

/// Whether `a` sorts before `b` by the chosen rank and then the name.
const fn comes_first(left: &Slice, right: &Slice, by_synopsis: bool) -> bool {
    let (rank_left, rank_right) = if by_synopsis {
        (left.synopsis_rank, right.synopsis_rank)
    } else {
        (left.description_rank, right.description_rank)
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

/// The indices of [`SLICES`] in help order (an insertion sort: the list is
/// short and `const` evaluation has no `sort`).
const fn help_order(by_synopsis: bool) -> [usize; SLICE_COUNT] {
    let mut order = [0usize; SLICE_COUNT];
    let mut i = 0;
    while i < SLICE_COUNT {
        let mut at = i;
        while at > 0 && comes_first(&SLICES[i], &SLICES[order[at - 1]], by_synopsis) {
            order[at] = order[at - 1];
            at -= 1;
        }
        order[at] = i;
        i += 1;
    }
    order
}

const SYNOPSIS_ORDER: [usize; SLICE_COUNT] = help_order(true);
const DESCRIPTION_ORDER: [usize; SLICE_COUNT] = help_order(false);

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
        let commands = SLICES[DESCRIPTION_ORDER[i]].commands;
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
    use std::collections::HashSet;
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

    #[test]
    fn the_readme_has_no_hand_maintained_command_list() {
        let readme = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
            .expect("README.md");
        assert!(
            !readme.contains("\ntoolkit skills list [ROOT] [--json]\n"),
            "the command list is generated: `toolkit help` prints it from the slice registrations"
        );
        assert!(readme.contains("`toolkit help`"));
    }
}
