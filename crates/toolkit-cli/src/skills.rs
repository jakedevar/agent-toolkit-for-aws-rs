//! Registration of `toolkit skills list` (the handler lives in `lib.rs`).

/// Registration of the `skills` slice: its routes and help text (see
/// `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "skills",
    commands: &[crate::CommandSpec {
        path: &["skills", "list"],
        flags: &["--json"],
        value_flags: &[],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: crate::skills_list,
    }],
    synopsis: r"  toolkit skills list [ROOT] [--json]
",
    description: r"  skills list   List skill directories (directories containing SKILL.md) under
                ROOT, sorted by name then relative path. ROOT defaults to
                `skills` in the current directory.
",
    exit_notes: "",
    doc: "crates/toolkit-cli/README.md",
    synopsis_rank: 10,
    description_rank: 10,
};
