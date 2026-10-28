# Command registration per slice (#4576)

Before this change every `toolkit` slice edited the same lines: the `COMMANDS`
table and the hand-written `USAGE` text in `crates/toolkit-cli/src/lib.rs`, the
command list in `crates/toolkit-cli/README.md` and the `mod`/`pub use` block of
`startup.rs`. Accepted slice heads therefore conflicted pairwise and each needed
an integration round. A slice now owns its registration and the shared list has
one line per slice, in sorted order.

## How to add a slice

1. In the module that holds your handlers, define

   ```rust
   pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
       name: "my-slice",                      // unique, lowercase, '-' separated
       commands: &[crate::CommandSpec { /* path, flags, value_flags, ... */ }],
       synopsis: r#"  toolkit my-slice run INPUT [--json]
   "#,                                       // lines for `Usage:`
       description: r#"  my-slice run  What it does. See docs/migration/my-slice.md.
   "#,                                       // block for `Commands:`
       exit_notes: "",                       // extra `Exit codes:` lines, if any
       doc: "docs/migration/my-slice.md",    // names every command path
       synopsis_rank: crate::registry::NEW_RANK,
       description_rank: crate::registry::NEW_RANK,
   };
   ```

   Routes keep their order (the first match wins), so list a shorter path after
   the longer paths it prefixes. A slice may define several consts (a slice per
   command family; see `cloudwatch::API_SLICE` and `DOMAIN_SLICE`).
2. Add `mod my_slice;` to `lib.rs` (or `pub(crate) mod` inside `startup.rs`).
3. Add one line `crate::my_slice::SLICE,` to `SLICES` in
   `crates/toolkit-cli/src/registry.rs`, at its alphabetical place by `name`.
4. Write the documentation in your own file, `docs/migration/my-slice.md`, and
   name it in `doc`. Do not append to the README command list (there is none).

Nothing else changes: `COMMANDS` (the routing table) and `USAGE` (the help
text) are assembled from `SLICES` at compile time, and `toolkit help` is the
generated command list. Slices added later are listed after the earlier ones,
alphabetically (both ranks `NEW_RANK`); the ranks of the earlier slices only
keep their help text in its historical order.

## What the tests enforce (`registry.rs`)

- `SLICES` is strictly ascending by `name` (so concurrent additions touch
  different lines and a mis-sorted or duplicated entry fails the build);
- no route is registered twice and none is shadowed by an earlier route;
- each slice keeps its declared route order, lists every route in its own
  synopsis, and its help fragments are well formed;
- `USAGE` is the ordered fragments;
- every command path is mentioned in the slice's `doc` file (drift between
  the registration and the documentation fails the test).

## Remaining shared lines

`mod x;` in `lib.rs` (one line, sorted) and the line in `SLICES` are the only
shared edits per slice, plus `Cargo.toml`/`Cargo.lock` when a slice adds a
workspace member (prefer an existing crate). Edits to the shared option and
exit-code text in `registry.rs` (`USAGE_TAIL`) are for options that belong to
no single slice.
