//! `toolkit startup skill-refs`: check that every file path the skills'
//! instructions mention exists (port of
//! `plugins/aws-startup-advisor/tools/check-skill-refs.py`, #4407).
//!
//! The aws-startup-advisor skills are prose state machines: a phase file says
//! "Load `references/phases/design/design-ai.md`" and the agent follows the
//! string. This gate makes a renamed or never-written target visible, and
//! makes a file move provable.
//!
//! # What counts as a reference
//!
//! A token in a skill or agent Markdown file that looks like a repository path
//! to an instruction or data file: `references/…`, `knowledge/…`,
//! `scripts/…`, `skills/…`, `agents/…`, `data/…`, `phases/…`, `shared/…`,
//! `design-refs/…`, `fixtures/…`, `tools/…`, with an extension in
//! md/json/py/sh/tf/yml/yaml/html/txt/toml and optional prefixes
//! `$PLUGIN_ROOT/`, `${CLAUDE_PLUGIN_ROOT}/`, `$SKILL_BASE/`, `<SKILL_BASE>/`,
//! `$GCP_BASE/`, `./`, `../`. Run-artifact paths (`$MIGRATION_DIR/…`,
//! `.migration/…`, `terraform/…`), templated tokens (`<slug>`, `{name}`,
//! `*`, `NNN`) and URLs are not references.
//!
//! # Resolution order
//!
//! An explicit prefix is authoritative (the agent runs the path as written);
//! otherwise the skill root, the referencing file's directory, the skill's
//! `references/`, the plugin and the repository root are tried, plus the
//! shared-library, gcp-dependent and agent rules documented on
//! [`Tree::candidates`]. A path that resolves nowhere is `MISSING_REF`.
//!
//! The Python regular expressions are replaced by exact hand-written scanners
//! that run in time linear in the length of a line (the Python pattern is
//! quadratic on a long run of path characters). Links are refused, never
//! followed: a symbolic link met while scanning or resolving is an error.

use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use toolkit_core::{neutralize_line, read_regular_file, Json, ToolkitError};

use super::artifacts::patterns::fnmatch;
use super::artifacts::pyjson::{dumps, loads};
use super::{
    is_regular_file, is_regular_file_under, read_text, read_text_under, MAX_ARTIFACT_BYTES,
};
use crate::manifests::{is_py_space, py_splitlines, resolve_lenient, universal_newlines};
use crate::{Invocation, Output};

/// Registration of the `startup-skill-refs` slice: its routes and help text (see `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "startup-skill-refs",
    commands: &[crate::CommandSpec {
        path: &["startup", "skill-refs"],
        flags: &["--check", "--list", "--json", "--no-baseline"],
        value_flags: &["--plugin-root", "--baseline"],
        strict_values: true,
        repeatable_flags: &[],
        max_positionals: 0,
        passthrough: false,
        handler: crate::startup::skill_refs_command,
    }],
    synopsis: r"  toolkit startup skill-refs (--check | --list) [--plugin-root DIR] [--baseline FILE]
                            [--no-baseline] [--json]
",
    description: r"  startup skill-refs (--check | --list)
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
",
    exit_notes: "",
    doc: "docs/migration/startup-tools.md",
    synopsis_rank: 170,
    description_rank: 170,
};

const ROOTS: [&str; 11] = [
    "references",
    "knowledge",
    "scripts",
    "skills",
    "agents",
    "data",
    "phases",
    "shared",
    "design-refs",
    "fixtures",
    "tools",
];
const EXTS: [&str; 10] = [
    "md", "json", "py", "sh", "tf", "yml", "yaml", "html", "txt", "toml",
];
/// Directories of run-artifact variables: `$NAME/` or `${NAME}/`.
const SKIP_VARIABLES: [&str; 11] = [
    "MIGRATION_DIR",
    "RUN_DIR",
    "BEDROCK_RUN_DIR",
    "REPO",
    "PHASE_DIR",
    "PLAN_DIR",
    "OUT",
    "TARGET",
    "APP",
    "WORKSPACE",
    "TMP",
];
/// Run-artifact placeholders: `<NAME>/`.
const SKIP_PLACEHOLDERS: [&str; 8] = [
    "PLAN_DIR",
    "MIGRATION_DIR",
    "RUN_DIR",
    "REPO",
    "run_dir",
    "migration_dir",
    "path",
    "repo",
];
/// Substrings that mark a generated-output path.
const SKIP_LITERALS: [&str; 7] = [
    ".migration/",
    ".saws-migrate/",
    "terraform/",
    "ai-migration/",
    "poc/",
    "scaffold/",
    ".github/",
];
/// Skills that execute gcp-to-aws's phase files and may name its tree.
const GCP_DEPENDENTS: [&str; 2] = ["agent-advisor", "llm-to-bedrock"];
/// The skill whose subagents live in `agents/`.
const AGENTS_OWNER: &str = "llm-to-bedrock";
/// Directories the scan never enters.
const SKIPPED_DIRS: [&str; 3] = ["node_modules", ".git", "fixtures"];

// ---- line scanner ----------------------------------------------------------

/// A path token found on one line, as character offsets.
#[derive(Debug, PartialEq, Eq)]
struct Token {
    start: usize,
    prefix_end: usize,
    end: usize,
}

fn is_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')
}

/// The `(?![A-Za-z0-9_.])` lookahead class: `-` is allowed to follow.
fn is_look_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.')
}

fn is_variable_char(c: char) -> bool {
    c.is_ascii_uppercase() || c == '_'
}

fn starts_with_at(chars: &[char], at: usize, word: &str) -> bool {
    let mut position = at;
    for expected in word.chars() {
        if chars.get(position) != Some(&expected) {
            return false;
        }
        position += 1;
    }
    true
}

/// Length of `$VAR/`, `${VAR}/` at `at`, or 0.
fn variable_item(chars: &[char], at: usize) -> usize {
    if chars.get(at) != Some(&'$') {
        return 0;
    }
    let mut cursor = at + 1;
    if chars.get(cursor) == Some(&'{') {
        cursor += 1;
    }
    let name_start = cursor;
    while chars.get(cursor).copied().is_some_and(is_variable_char) {
        cursor += 1;
    }
    if cursor == name_start {
        return 0;
    }
    if chars.get(cursor) == Some(&'}') {
        cursor += 1;
    }
    if chars.get(cursor) == Some(&'/') {
        cursor + 1 - at
    } else {
        0
    }
}

/// Length of `<VAR>/` at `at`, or 0.
fn placeholder_item(chars: &[char], at: usize) -> usize {
    if chars.get(at) != Some(&'<') {
        return 0;
    }
    let mut cursor = at + 1;
    let name_start = cursor;
    while chars.get(cursor).copied().is_some_and(is_variable_char) {
        cursor += 1;
    }
    if cursor == name_start
        || chars.get(cursor) != Some(&'>')
        || chars.get(cursor + 1) != Some(&'/')
    {
        return 0;
    }
    cursor + 2 - at
}

/// Length of one prefix item at `at` (`$VAR/`, `<VAR>/`, `./`, `../`), or 0.
fn prefix_item(chars: &[char], at: usize) -> usize {
    match chars.get(at) {
        Some('$') => variable_item(chars, at),
        Some('<') => placeholder_item(chars, at),
        Some('.') if chars.get(at + 1) == Some(&'/') => 2,
        Some('.') if chars.get(at + 1) == Some(&'.') && chars.get(at + 2) == Some(&'/') => 3,
        _ => 0,
    }
}

/// Length of `.ext` at `at` when an extension follows and the next character
/// does not continue the name, or `None`.
fn extension_at(chars: &[char], at: usize) -> Option<usize> {
    if chars.get(at) != Some(&'.') {
        return None;
    }
    EXTS.iter().find_map(|ext| {
        let end = at + 1 + ext.len();
        (starts_with_at(chars, at + 1, ext) && !chars.get(end).copied().is_some_and(is_look_char))
            .then_some(1 + ext.len())
    })
}

/// Finds the path tokens of one line, as `re.finditer` of
/// `PREFIX path\.EXT (?![A-Za-z0-9_.])` would.
///
/// The path is `seg(/seg)*\.ext` with `seg` one or more of `[A-Za-z0-9_.-]`;
/// the regular expression backtracks to the LAST position where `.ext` can
/// end the path, which is what the tables below look up in constant time.
fn find_tokens(chars: &[char]) -> Vec<Token> {
    let n = chars.len();
    // region_end[i]: end of the longest `seg(/seg)*` starting at path char i.
    let mut region_end = vec![0_usize; n + 1];
    // chain_end[i]: end of the prefix items chained from i (i when none).
    let mut chain_end = vec![0_usize; n + 1];
    chain_end[n] = n;
    region_end[n] = n;
    for i in (0..n).rev() {
        region_end[i] = if !is_path_char(chars[i]) {
            i
        } else if chars.get(i + 1).copied().is_some_and(is_path_char) {
            region_end[i + 1]
        } else if chars.get(i + 1) == Some(&'/')
            && chars.get(i + 2).copied().is_some_and(is_path_char)
        {
            region_end[i + 2]
        } else {
            i + 1
        };
        let item = prefix_item(chars, i);
        chain_end[i] = if item == 0 { i } else { chain_end[i + item] };
    }
    // last_cut[k]: the largest e <= k where the path body may end and `.ext`
    // follows (the body must not end with `/`).
    let mut last_cut: Vec<Option<(usize, usize)>> = vec![None; n + 1];
    for e in 0..n {
        let here = if e > 0 && chars[e - 1] != '/' {
            extension_at(chars, e).map(|len| (e, len))
        } else {
            None
        };
        last_cut[e] = here.or(if e > 0 { last_cut[e - 1] } else { None });
    }

    let mut tokens = Vec::new();
    let mut at = 0;
    while at < n {
        let path_start = chain_end[at];
        let found = if path_start < n && is_path_char(chars[path_start]) {
            let region = region_end[path_start];
            last_cut[region - 1].filter(|(cut, _)| *cut > path_start)
        } else {
            None
        };
        if let Some((cut, length)) = found {
            let end = cut + length;
            tokens.push(Token {
                start: at,
                prefix_end: path_start,
                end,
            });
            at = end;
        } else {
            at += 1;
        }
    }
    tokens
}

/// `_URL_RE.sub("", line)`: removes `http://…` and `https://…` up to the next
/// whitespace.
fn strip_urls(chars: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(chars.len());
    let mut at = 0;
    while at < chars.len() {
        let scheme = if starts_with_at(chars, at, "https://") {
            8
        } else if starts_with_at(chars, at, "http://") {
            7
        } else {
            0
        };
        if scheme == 0 {
            out.push(chars[at]);
            at += 1;
        } else {
            at += scheme;
            while chars.get(at).is_some_and(|c| !is_py_space(*c)) {
                at += 1;
            }
        }
    }
    out
}

/// The smallest end of a skip-prefix match that starts at `at` in `text`
/// (`_SKIP_PREFIX_RE`): a run-artifact variable, placeholder or
/// generated-output directory. Constant work: the alternatives are fixed.
fn skip_match_end(text: &[char], at: usize) -> Option<usize> {
    fn note(best: &mut Option<usize>, end: usize) {
        *best = Some(best.map_or(end, |b| b.min(end)));
    }
    let mut best = None;
    match text[at] {
        '$' => {
            let mut cursor = at + 1;
            if text.get(cursor) == Some(&'{') {
                cursor += 1;
            }
            for name in SKIP_VARIABLES {
                if starts_with_at(text, cursor, name) {
                    let mut end = cursor + name.len();
                    if text.get(end) == Some(&'}') {
                        end += 1;
                    }
                    if text.get(end) == Some(&'/') {
                        note(&mut best, end + 1);
                    }
                }
            }
        }
        '<' => {
            for name in SKIP_PLACEHOLDERS {
                if starts_with_at(text, at + 1, name)
                    && text.get(at + 1 + name.len()) == Some(&'>')
                    && text.get(at + 2 + name.len()) == Some(&'/')
                {
                    note(&mut best, at + 3 + name.len());
                }
            }
        }
        _ => {}
    }
    for literal in SKIP_LITERALS {
        if starts_with_at(text, at, literal) {
            note(&mut best, at + literal.len());
        }
    }
    best
}

/// The line-wide tables that turn each token's context question into a
/// constant-time lookup, so a line is scanned in one forward pass however
/// many tokens share one run of non-space characters.
struct LineIndex {
    /// `run_start[i]`: where the run of characters that are neither whitespace
    /// nor a backtick (the regular expression `[^\s\x60]*`) that ends just
    /// before position `i` begins: the start of the context of a token at `i`.
    run_start: Vec<usize>,
    /// `skip_end[i]`: the smallest end of a skip-prefix match that starts at
    /// or after `i` (`usize::MAX` when there is none).
    skip_end: Vec<usize>,
}

impl LineIndex {
    fn new(chars: &[char], work: &mut u64) -> Self {
        let n = chars.len();
        let mut run_start = Vec::with_capacity(n + 1);
        run_start.push(0);
        for (at, c) in chars.iter().enumerate() {
            run_start.push(if is_py_space(*c) || *c == '`' {
                at + 1
            } else {
                run_start[at]
            });
        }
        let mut skip_end = vec![usize::MAX; n + 1];
        for at in (0..n).rev() {
            skip_end[at] =
                skip_match_end(chars, at).map_or(skip_end[at + 1], |end| end.min(skip_end[at + 1]));
        }
        // One step per position for each table; the skip test at a position
        // looks at a fixed number of names.
        *work += 2 * (n as u64 + 1);
        Self {
            run_start,
            skip_end,
        }
    }

    /// `_SKIP_PREFIX_RE.search(context + prefix)` for the token at `start`
    /// whose prefix ends at `prefix_end`. The context and the prefix are the
    /// adjacent characters `chars[run_start[start]..prefix_end]`, and a match
    /// inside that window is a match of the line that ends by `prefix_end`.
    fn skipped(&self, start: usize, prefix_end: usize) -> bool {
        self.skip_end[self.run_start[start]] <= prefix_end
    }
}

/// The tokens of one line that survive every filter that depends on the line
/// alone, in order. `work` counts the steps taken: a pass over the line and,
/// per token, its own length, never the text before it.
fn line_candidates(chars: &[char], work: &mut u64) -> Vec<Token> {
    let mut tokens = find_tokens(chars);
    *work += chars.len() as u64;
    if tokens.is_empty() {
        return tokens;
    }
    let index = LineIndex::new(chars, work);
    tokens.retain(|token| {
        let prefix = &chars[token.start..token.prefix_end];
        let full = &chars[token.start..token.end];
        *work += full.len() as u64 + 1;
        if index.skipped(token.start, token.prefix_end) || is_templated(full) {
            return false;
        }
        let path: String = chars[token.prefix_end..token.end].iter().collect();
        if prefix.is_empty() && !starts_with_root(&path) {
            return false;
        }
        // `./x.sh`, `../plan.md`: a relative token with no known root
        // directory in it is an output or working-tree path.
        if is_relative_only(prefix) && !has_root_segment(&path) {
            return false;
        }
        !(token.start > 0 && glues(chars[token.start - 1]))
    });
    tokens
}

/// `_is_templated`: after stripping leading `<VAR>/` and `$VAR/` items, the
/// token holds a template character, `NNN` or `/…`.
fn is_templated(token: &[char]) -> bool {
    let mut at = 0;
    loop {
        let item = match token.get(at) {
            Some('$') => variable_item(token, at),
            Some('<') => placeholder_item(token, at),
            _ => 0,
        };
        if item == 0 {
            break;
        }
        at += item;
    }
    let body = &token[at..];
    body.iter().any(|c| "<>{}*[]".contains(*c))
        || body.windows(3).any(|w| w == ['N', 'N', 'N'])
        || body.windows(2).any(|w| w == ['/', '…'])
}

/// The prefix consists only of `./` and `../` items.
fn is_relative_only(prefix: &[char]) -> bool {
    let mut at = 0;
    while at < prefix.len() {
        let item = match (prefix.get(at), prefix.get(at + 1), prefix.get(at + 2)) {
            (Some('.'), Some('/'), _) => 2,
            (Some('.'), Some('.'), Some('/')) => 3,
            _ => return false,
        };
        at += item;
    }
    !prefix.is_empty()
}

fn starts_with_root(path: &str) -> bool {
    ROOTS.iter().any(|root| {
        path.strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// `(^|/)ROOT/` anywhere in `path`: some non-final segment is a root name.
fn has_root_segment(path: &str) -> bool {
    let mut segments: Vec<&str> = path.split('/').collect();
    segments.pop();
    segments.iter().any(|segment| ROOTS.contains(segment))
}

/// Python `str.isalnum()` for the character before a token.
fn glues(previous: char) -> bool {
    previous.is_alphanumeric() || matches!(previous, '_' | '-' | '/')
}

// ---- tree and resolution ----------------------------------------------------

/// One reference found in a file.
#[derive(Clone, Debug)]
pub(super) struct Reference {
    /// The path of the file, relative to the plugin (`/` separated).
    pub(super) source: String,
    pub(super) line: usize,
    /// The token as written.
    pub(super) raw: String,
    pub(super) resolved: Option<PathBuf>,
}

impl Reference {
    /// The skill the file belongs to: `skills/<skill>/…` → `<skill>`,
    /// `agents/…` → `agents`, anything else `?`.
    fn skill(&self) -> String {
        let parts: Vec<&str> = self.source.split('/').collect();
        if parts.first() == Some(&"skills") {
            if let Some(name) = parts.get(1) {
                return (*name).to_string();
            }
        }
        if parts.contains(&"agents") {
            "agents".to_string()
        } else {
            "?".to_string()
        }
    }
}

/// The plugin being checked, its repository root and the directory listings
/// the resolver needs (built once, on demand).
pub(super) struct Tree {
    plugin: PathBuf,
    repo: PathBuf,
    skill_dirs: Option<Vec<PathBuf>>,
    files_below: HashMap<PathBuf, Vec<PathBuf>>,
    /// Steps taken scanning lines (see `line_candidates`); the tests bound it.
    work: u64,
}

fn link_error(path: &Path) -> String {
    format!(
        "cannot read {}: symbolic links are not followed",
        path.display()
    )
}

/// Lexical `Path.resolve()` for a tree that holds no links: `.` and `..` are
/// folded away.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

impl Tree {
    pub(super) fn new(plugin: &Path) -> Self {
        let repo = plugin
            .parent()
            .and_then(Path::parent)
            .unwrap_or(plugin)
            .to_path_buf();
        Self {
            plugin: plugin.to_path_buf(),
            repo,
            skill_dirs: None,
            files_below: HashMap::new(),
            work: 0,
        }
    }

    /// `_rel`: the path relative to the repository root, else as it is.
    pub(super) fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.repo).map_or_else(
            |_| path.display().to_string(),
            |tail| {
                tail.components()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/")
            },
        )
    }

    /// The regular file at `path`, if there is one. A link at any component
    /// below the repository root is an error.
    fn regular(&self, path: &Path) -> Result<Option<PathBuf>, String> {
        let normal = normalize(path);
        Ok(is_regular_file_under(&self.repo, &normal)?.then_some(normal))
    }

    /// The sorted directories directly below `skills/`.
    fn skill_dirs(&mut self) -> Result<Vec<PathBuf>, String> {
        if self.skill_dirs.is_none() {
            let skills = self.plugin.join("skills");
            let mut dirs = Vec::new();
            if let Ok(entries) = fs::read_dir(&skills) {
                for entry in entries.flatten() {
                    let kind = entry.file_type().map_err(|e| e.to_string())?;
                    if kind.is_symlink() {
                        return Err(link_error(&entry.path()));
                    }
                    if kind.is_dir() {
                        dirs.push(entry.path());
                    }
                }
            }
            dirs.sort();
            self.skill_dirs = Some(dirs);
        }
        Ok(self.skill_dirs.clone().unwrap_or_default())
    }

    /// Every regular file below `dir` (`dir.rglob(name)` is a filter of this),
    /// sorted.
    fn files_below(&mut self, dir: &Path) -> Result<&[PathBuf], String> {
        if !self.files_below.contains_key(dir) {
            let mut files = Vec::new();
            match fs::symlink_metadata(dir) {
                Ok(meta) if meta.file_type().is_symlink() => return Err(link_error(dir)),
                Ok(meta) if meta.is_dir() => {
                    walk(dir, &mut |path| files.push(path.to_path_buf()), &[])?;
                }
                _ => {}
            }
            files.sort();
            self.files_below.insert(dir.to_path_buf(), files);
        }
        Ok(self.files_below.get(dir).map_or(&[], Vec::as_slice))
    }

    /// Files below `dir` named `name` whose full path ends with `suffix`.
    fn named_below(
        &mut self,
        dir: &Path,
        name: &str,
        suffix: &str,
    ) -> Result<Vec<PathBuf>, String> {
        Ok(self
            .files_below(dir)?
            .iter()
            .filter(|file| {
                file.file_name().is_some_and(|n| n == name)
                    && file.to_string_lossy().ends_with(suffix)
            })
            .cloned()
            .collect())
    }

    /// The ordered locations a token could mean.
    ///
    /// 1. An explicit prefix names the location and nothing else is tried:
    ///    `$PLUGIN_ROOT/` the plugin, `$GCP_BASE/` the gcp-to-aws skill,
    ///    `$SKILL_BASE/` or `<SKILL_BASE>/` the skill (`../` steps up from
    ///    it), `$HELPERS/` and `$SCRIPTS/` their skill directories, `./` and
    ///    `../` the referencing file's directory.
    /// 2. Otherwise: the skill root, the file's directory, the skill's
    ///    `references/`, the plugin and the repository root; for the shared
    ///    library and its vendored copies also `skills/shared` (with the
    ///    `vendored/`/`shared/` lead stripped) and every skill; for the gcp
    ///    dependents the gcp-to-aws tree; for `agents/` only llm-to-bedrock.
    fn candidates(
        &mut self,
        prefix: &str,
        path: &str,
        source: &Path,
        source_relative: &str,
    ) -> Result<Vec<PathBuf>, String> {
        let skills = self.plugin.join("skills");
        let skill: Option<PathBuf> = Path::new(source_relative)
            .strip_prefix("skills")
            .ok()
            .and_then(|tail| tail.components().next())
            .map(|first| skills.join(first.as_os_str()));
        let here = source.parent().unwrap_or(&self.plugin).to_path_buf();
        let mut found: Vec<PathBuf> = Vec::new();
        if prefix.contains("PLUGIN_ROOT") {
            found.push(self.plugin.join(path));
        } else if prefix.contains("GCP_BASE") {
            found.push(skills.join("gcp-to-aws").join(path));
        } else if prefix.contains("SKILL_BASE") || prefix.contains("SKILL_DIR") {
            found.push(skill.as_deref().unwrap_or(&here).join(path));
        } else if prefix.contains("HELPERS") && skill.is_some() {
            if let Some(skill) = &skill {
                found.push(skill.join("references").join("helpers").join(path));
            }
        } else if prefix.contains("SCRIPTS") && skill.is_some() {
            if let Some(skill) = &skill {
                found.push(skill.join("scripts").join(path));
            }
        } else if prefix.starts_with("./") || prefix.starts_with("../") {
            // The relative prefix is the whole location: the unprefixed tail
            // is never tried under the skill root.
            found.push(here.join(prefix).join(path));
        }
        if prefix.contains("../") && prefix.contains("SKILL_BASE") {
            if let Some(skill) = &skill {
                let mut up = skill.clone();
                for _ in 0..prefix.matches("../").count() {
                    up.pop();
                }
                found.push(up.join(path));
            }
        }
        if !found.is_empty() {
            return Ok(found);
        }
        if let Some(skill) = &skill {
            found.push(skill.join(path));
        }
        found.push(here.join(path));
        if let Some(skill) = &skill {
            found.push(skill.join("references").join(path));
        }
        found.push(self.plugin.join(path));
        found.push(self.repo.join(path));
        if is_library_origin(source_relative) {
            let shared = skills.join("shared");
            let stripped = strip_library_lead(path);
            found.push(shared.join(stripped));
            let name = stripped.rsplit('/').next().unwrap_or(stripped);
            found.extend(self.named_below(&shared, name, name)?);
            for other in self.skill_dirs()? {
                found.push(other.join(path));
                found.push(other.join("references").join(path));
            }
        }
        if let Some(skill) = &skill {
            if skill
                .file_name()
                .is_some_and(|n| GCP_DEPENDENTS.iter().any(|d| n == *d))
            {
                let gcp = skills.join("gcp-to-aws");
                found.push(gcp.join(path));
                found.push(gcp.join("references").join(path));
            }
        } else if source_relative.split('/').any(|part| part == "agents") {
            // agents/ holds llm-to-bedrock's subagents; their paths are
            // relative to its scripts/ or helper directories. Resolve ONLY
            // against that skill: a same-named file elsewhere must not make
            // a dead path look alive.
            let owner = skills.join(AGENTS_OWNER);
            found.push(owner.join(path));
            if fs::symlink_metadata(&owner).is_ok_and(|m| m.is_dir()) {
                let name = path.rsplit('/').next().unwrap_or(path);
                found.extend(self.named_below(&owner, name, path)?);
            }
        }
        Ok(found)
    }

    fn resolve(
        &mut self,
        prefix: &str,
        path: &str,
        source: &Path,
        source_relative: &str,
    ) -> Result<Option<PathBuf>, String> {
        for candidate in self.candidates(prefix, path, source, source_relative)? {
            if let Some(found) = self.regular(&candidate)? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    /// The references of one file. A file that cannot be read as UTF-8 text
    /// has none (as in the Python tool).
    pub(super) fn scan_file(
        &mut self,
        source: &Path,
        source_relative: &str,
    ) -> Result<Vec<Reference>, String> {
        let Ok(text) = read_regular_file(source, MAX_ARTIFACT_BYTES) else {
            return Ok(Vec::new());
        };
        let text = universal_newlines(&text);
        let mut references = Vec::new();
        for (index, line) in py_splitlines(&text).into_iter().enumerate() {
            let chars = strip_urls(&line.chars().collect::<Vec<_>>());
            for token in line_candidates(&chars, &mut self.work) {
                let prefix: String = chars[token.start..token.prefix_end].iter().collect();
                let path: String = chars[token.prefix_end..token.end].iter().collect();
                let resolved = self.resolve(&prefix, &path, source, source_relative)?;
                references.push(Reference {
                    source: source_relative.to_string(),
                    line: index + 1,
                    raw: chars[token.start..token.end].iter().collect(),
                    resolved,
                });
            }
        }
        Ok(references)
    }

    /// Every reference of the plugin's `skills/` and `agents/` Markdown files,
    /// in path order.
    pub(super) fn scan(&mut self) -> Result<Vec<Reference>, String> {
        let mut files: Vec<PathBuf> = Vec::new();
        for sub in ["skills", "agents"] {
            let root = self.plugin.join(sub);
            match fs::symlink_metadata(&root) {
                Ok(meta) if meta.file_type().is_symlink() => return Err(link_error(&root)),
                Ok(meta) if meta.is_dir() => {
                    walk(
                        &root,
                        &mut |path| {
                            if path.extension().is_some_and(|e| e == "md") {
                                files.push(path.to_path_buf());
                            }
                        },
                        &SKIPPED_DIRS,
                    )?;
                }
                _ => {}
            }
        }
        files.sort();
        let mut references = Vec::new();
        for file in files {
            let relative = file
                .strip_prefix(&self.plugin)
                .map(|tail| {
                    tail.components()
                        .map(|part| part.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/")
                })
                .unwrap_or_default();
            references.extend(self.scan_file(&file, &relative)?);
        }
        Ok(references)
    }
}

/// `_LIBRARY_ORIGIN_RE.search("/" + path)`: `/skills/shared/` or
/// `/skills/<skill>/references/(vendored|shared)/` anywhere in the path.
fn is_library_origin(relative: &str) -> bool {
    let text = format!("/{relative}");
    text.match_indices("/skills/").any(|(at, marker)| {
        let rest = &text[at + marker.len()..];
        rest.starts_with("shared/") || {
            let mut parts = rest.splitn(4, '/');
            matches!(
                (parts.next(), parts.next(), parts.next(), parts.next()),
                (Some(skill), Some("references"), Some("vendored" | "shared"), Some(_))
                    if !skill.is_empty()
            )
        }
    })
}

/// `re.sub(r"^(references/)?(vendored|shared)/", "", path)`.
fn strip_library_lead(path: &str) -> &str {
    let rest = path.strip_prefix("references/").unwrap_or(path);
    for lead in ["vendored/", "shared/"] {
        if let Some(tail) = rest.strip_prefix(lead) {
            return tail;
        }
    }
    path
}

/// Calls `visit` for each regular file below `dir`; a link is an error.
/// Directories named in `skipped` are not entered.
fn walk(dir: &Path, visit: &mut dyn FnMut(&Path), skipped: &[&str]) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if skipped.iter().any(|name| entry.file_name() == *name) {
            continue;
        }
        if kind.is_symlink() {
            return Err(link_error(&path));
        }
        if kind.is_dir() {
            walk(&path, visit, skipped)?;
        } else if kind.is_file() {
            visit(&path);
        }
    }
    Ok(())
}

// ---- baseline ---------------------------------------------------------------

/// One `ignore` rule or `entries` item: glob patterns for the source file and
/// the token.
struct Rule {
    source: String,
    reference: String,
    json: Json,
}

fn rules(baseline: &Json, key: &str) -> Result<Vec<Rule>, String> {
    let items = match baseline.get(key) {
        None => return Ok(Vec::new()),
        Some(Json::Array(items)) => items,
        Some(_) => return Err(format!("invalid baseline: '{key}' must be an array")),
    };
    let mut out = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if !matches!(item, Json::Object(_)) {
            return Err(format!(
                "invalid baseline: {key}[{index}] must be an object"
            ));
        }
        let pattern = |field: &str| -> Result<String, String> {
            match item.get(field) {
                None => Ok("*".to_string()),
                Some(Json::String(text)) => Ok(text.clone()),
                Some(_) => Err(format!(
                    "invalid baseline: {key}[{index}].{field} must be a string"
                )),
            }
        };
        out.push(Rule {
            source: pattern("source")?,
            reference: pattern("ref")?,
            json: item.clone(),
        });
    }
    Ok(out)
}

/// (unbaselined, baselined, stale `entries`) of [`apply_baseline`].
type BaselineSplit = (Vec<Reference>, Vec<Reference>, Vec<Json>);

/// Splits the missing references into (unbaselined, baselined) and returns the
/// `entries` that matched nothing.
///
/// - `ignore[]`: permanent policy for tokens that are not repository
///   references (paths the skill generates, example paths in a user's app).
///   Never stale; matching references are dropped silently.
/// - `entries[]`: known dead references with the fix that retires each one.
///   STALE when they stop matching, so the list only shrinks.
fn apply_baseline(
    tree: &Tree,
    mut missing: Vec<Reference>,
    baseline: &Json,
) -> Result<BaselineSplit, String> {
    if !matches!(baseline, Json::Object(_)) {
        return Err("invalid baseline: the document must be a JSON object".to_string());
    }
    for rule in rules(baseline, "ignore")? {
        missing.retain(|r| {
            !(fnmatch(&source_shown(tree, r), &rule.source) && fnmatch(&r.raw, &rule.reference))
        });
    }
    let entries = rules(baseline, "entries")?;
    let mut used = vec![false; entries.len()];
    let (mut unbaselined, mut baselined) = (Vec::new(), Vec::new());
    for reference in missing {
        let hit = entries.iter().position(|e| {
            fnmatch(&source_shown(tree, &reference), &e.source)
                && fnmatch(&reference.raw, &e.reference)
        });
        match hit {
            Some(index) => {
                used[index] = true;
                baselined.push(reference);
            }
            None => unbaselined.push(reference),
        }
    }
    let stale = entries
        .into_iter()
        .zip(used)
        .filter(|(_, matched)| !matched)
        .map(|(entry, _)| entry.json)
        .collect();
    Ok((unbaselined, baselined, stale))
}

// ---- command ----------------------------------------------------------------

/// `text` as one safe line for stdout.
fn shown(text: &str) -> String {
    neutralize_line(text).into_owned()
}

/// JSON text with the one character `ensure_ascii` leaves raw and a terminal
/// must not receive (DEL) written as an escape.
fn json_text(value: &Json) -> String {
    dumps(value, Some(2), false).replace('\u{7f}', "\\u007f")
}

/// The source path as the report shows it: relative to the repository root.
fn source_shown(tree: &Tree, reference: &Reference) -> String {
    tree.relative(&tree.plugin.join(&reference.source))
}

fn missing_json(tree: &Tree, reference: &Reference) -> Json {
    Json::object(vec![
        ("source", Json::string(source_shown(tree, reference))),
        (
            "line",
            Json::Int(i64::try_from(reference.line).unwrap_or(i64::MAX)),
        ),
        ("ref", Json::string(reference.raw.as_str())),
        ("skill", Json::string(reference.skill())),
    ])
}

fn baselined_json(tree: &Tree, reference: &Reference) -> Json {
    Json::object(vec![
        ("source", Json::string(source_shown(tree, reference))),
        (
            "line",
            Json::Int(i64::try_from(reference.line).unwrap_or(i64::MAX)),
        ),
        ("ref", Json::string(reference.raw.as_str())),
    ])
}

/// Handler for `startup skill-refs`.
///
/// # Errors
///
/// A usage error (exit 2) when neither `--check` nor `--list` is given or the
/// plugin directory is missing; a runtime error (exit 1) when a file cannot be
/// read safely, the baseline is malformed, or `--check` finds a non-baselined
/// missing reference or a stale baseline entry.
#[allow(clippy::too_many_lines)]
pub fn skill_refs_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let check = invocation.has_flag("--check");
    let list = invocation.has_flag("--list");
    if !check && !list {
        return Err(ToolkitError::usage(
            "skill_refs_mode",
            "pass --check or --list",
        ));
    }
    let plugin = resolve_lenient(Path::new(invocation.value("--plugin-root").unwrap_or(".")));
    if !plugin.is_dir() {
        return Err(ToolkitError::usage(
            "plugin_not_found",
            format!(
                "Plugin directory not found: {}",
                shown(&plugin.display().to_string())
            ),
        ));
    }
    let failed =
        |message: String| ToolkitError::runtime("skill_refs_input_invalid", shown(&message));
    let mut tree = Tree::new(&plugin);
    let references = tree.scan().map_err(failed)?;

    if list {
        for reference in &references {
            let target = reference
                .resolved
                .as_ref()
                .map_or_else(|| "MISSING".to_string(), |path| tree.relative(path));
            output.line(&shown(&format!(
                "{}:{}  {}  ->  {target}",
                source_shown(&tree, reference),
                reference.line,
                reference.raw
            )))?;
        }
        return Ok(());
    }

    let explicit = invocation.value("--baseline").map(PathBuf::from);
    let baseline_path = explicit
        .clone()
        .unwrap_or_else(|| plugin.join("tools").join("skill-refs-baseline.json"));
    let mut missing: Vec<Reference> = references
        .iter()
        .filter(|r| r.resolved.is_none())
        .cloned()
        .collect();
    let mut baselined: Vec<Reference> = Vec::new();
    let mut stale: Vec<Json> = Vec::new();
    if !invocation.has_flag("--no-baseline") {
        let present = if explicit.is_some() {
            is_regular_file(&baseline_path)
        } else {
            is_regular_file_under(&plugin, &baseline_path)
        }
        .map_err(failed)?;
        if present {
            let shown_path = baseline_path.display().to_string();
            let text = if explicit.is_some() {
                read_text(&baseline_path, &shown(&shown_path))
            } else {
                read_text_under(&plugin, &baseline_path, &shown(&shown_path))
            }
            .map_err(failed)?;
            let baseline =
                loads(&text).map_err(|e| failed(format!("{shown_path}: invalid JSON ({e})")))?;
            (missing, baselined, stale) =
                apply_baseline(&tree, missing, &baseline).map_err(failed)?;
        }
    }

    let problems = missing.len() + stale.len();
    if invocation.has_flag("--json") {
        let report = Json::object(vec![
            (
                "references",
                Json::Int(i64::try_from(references.len()).unwrap_or(i64::MAX)),
            ),
            (
                "missing",
                Json::Array(missing.iter().map(|r| missing_json(&tree, r)).collect()),
            ),
            (
                "baselined",
                Json::Array(baselined.iter().map(|r| baselined_json(&tree, r)).collect()),
            ),
            ("stale_baseline_entries", Json::Array(stale.clone())),
        ]);
        output.line(&json_text(&report))?;
    } else {
        for reference in &missing {
            output.line(&shown(&format!(
                "ERROR  MISSING_REF  {}:{}  `{}`",
                source_shown(&tree, reference),
                reference.line,
                reference.raw
            )))?;
        }
        for entry in &stale {
            output.line(&format!(
                "ERROR  STALE_BASELINE  entry matched nothing — remove it: {}",
                dumps(entry, None, false).replace('\u{7f}', "\\u007f")
            ))?;
        }
        if !baselined.is_empty() {
            output.line(&shown(&format!(
                "baselined (known dead references, explained in {}): {}",
                tree.relative(&baseline_path),
                baselined.len()
            )))?;
        }
        output.line(&format!(
            "\n{} — {} reference(s) checked, {} missing, {} baselined, {} stale baseline entr{}",
            if problems == 0 { "PASS" } else { "FAIL" },
            references.len(),
            missing.len(),
            baselined.len(),
            stale.len(),
            if stale.len() == 1 { "y" } else { "ies" }
        ))?;
    }
    if problems == 0 {
        Ok(())
    } else {
        Err(ToolkitError::runtime(
            "skill_refs_failed",
            format!("{problems} skill reference problem(s)"),
        ))
    }
}

#[cfg(test)]
mod tests;
