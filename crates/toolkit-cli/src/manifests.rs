//! `toolkit manifests validate`: the Rust replacement for `tools/validate.py`
//! (#4375).
//!
//! The command checks, offline and read-only, the three marketplace manifests,
//! every plugin's Claude/Codex/Cursor/Devin manifests and `.mcp.json`, the
//! `SKILL.md` frontmatter of plugin skills and of the top-level `skills/`
//! tree. Findings are collected in order as a [`Report`]; the command handler
//! prints progress lines on stdout and `  ERROR: <message>` lines on stderr and
//! fails with exit status 1 when any error was found.
//!
//! The rule set, message texts and ordering follow the Python validator. The
//! deliberate differences are recorded in `docs/migration/manifests-validator.md`
//! and the toolkit CLI README.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};

use toolkit_core::{
    parse_json, read_regular_file, Json, PathError, ToolkitError, MAX_TEXT_FILE_BYTES,
};

use crate::{Invocation, Output};

/// Deepest `skills/` nesting the walk follows (guards against pathological trees).
const MAX_WALK_DEPTH: usize = 64;

/// One ordered line of validator output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A `Validating ...` progress line (stdout).
    Progress(String),
    /// A finding (stderr as `  ERROR: <message>`).
    Error(String),
}

/// The ordered result of a validation run.
#[derive(Debug, Default)]
pub struct Report {
    pub(crate) events: Vec<Event>,
}

impl Report {
    /// All events in the order they were produced.
    #[must_use]
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// The error messages only, in order.
    #[must_use]
    pub fn errors(&self) -> Vec<&str> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::Error(text) => Some(text.as_str()),
                Event::Progress(_) => None,
            })
            .collect()
    }

    /// Number of errors.
    #[must_use]
    pub fn error_count(&self) -> usize {
        self.errors().len()
    }

    /// The progress lines only, in order.
    #[must_use]
    pub fn progress(&self) -> Vec<&str> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::Progress(text) => Some(text.as_str()),
                Event::Error(_) => None,
            })
            .collect()
    }
}

/// Validates the repository at `root`, or only `plugins/<plugin>` when
/// `plugin` is given.
///
/// # Errors
///
/// Returns a runtime error when `root` is missing or not a directory, or when
/// `plugin` does not name a directory directly under `root/plugins` (a name
/// with a path separator, `.` or `..` never does). Validation findings are
/// not errors here: they are in the returned [`Report`].
pub fn validate_tree(root: &Path, plugin: Option<&str>) -> Result<Report, ToolkitError> {
    match fs::metadata(root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(ToolkitError::runtime(
                "root_not_directory",
                format!("root is not a directory: {}", root.display()),
            ))
        }
        Err(_) => {
            return Err(ToolkitError::runtime(
                "root_not_found",
                format!("root does not exist: {}", root.display()),
            ))
        }
    }
    let mut validator = Validator::new(root);
    if let Some(name) = plugin {
        let single_component =
            !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0']);
        if !single_component || !root.join("plugins").join(name).is_dir() {
            return Err(ToolkitError::runtime(
                "plugin_not_found",
                format!("Plugin not found: {name}"),
            ));
        }
        validator.validate_plugin(name);
    } else {
        validator.validate_marketplace(".claude-plugin/marketplace.json", "Claude Code");
        validator.validate_marketplace(".agents/plugins/marketplace.json", "Codex");
        validator.validate_marketplace(".cursor-plugin/marketplace.json", "Cursor");
        if root.join("plugins").is_dir() {
            for name in validator.list_dir("plugins") {
                if root.join("plugins").join(&name).is_dir() {
                    validator.validate_plugin(&name);
                }
            }
        }
        validator.validate_top_level_skills();
    }
    Ok(validator.report)
}

/// Handler for `toolkit manifests validate [ROOT] [--plugin NAME]`.
///
/// # Errors
///
/// Fails with a runtime error for an unusable root or plugin name and with
/// `validation_failed` (exit status 1) when the report contains errors.
pub fn validate_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let root = PathBuf::from(invocation.positional(0).unwrap_or("."));
    let report = validate_tree(&root, invocation.value("--plugin"))?;
    for event in report.events() {
        match event {
            Event::Progress(text) => output.line(text)?,
            Event::Error(text) => output.diagnostic(&format!("  ERROR: {text}")),
        }
    }
    let count = report.error_count();
    if count > 0 {
        return Err(ToolkitError::runtime(
            "validation_failed",
            format!("Validation failed with {count} error(s)."),
        ));
    }
    output.line("")?;
    output.line("All validations passed.")
}

struct Validator<'a> {
    root: &'a Path,
    report: Report,
}

impl<'a> Validator<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            report: Report::default(),
        }
    }

    fn progress(&mut self, text: String) {
        self.report.events.push(Event::Progress(text));
    }

    fn error(&mut self, text: String) {
        self.report.events.push(Event::Error(text));
    }

    /// Sorted child names of the root-relative directory `rel`; a directory
    /// that cannot be listed is an error and yields no names.
    fn list_dir(&mut self, rel: &str) -> Vec<String> {
        match fs::read_dir(self.root.join(rel)) {
            Ok(entries) => {
                let mut names: Vec<String> = entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                names
            }
            Err(error) => {
                self.error(format!("Cannot list {rel}: {error}"));
                Vec::new()
            }
        }
    }

    fn read_text(&mut self, rel: &str) -> Option<String> {
        match read_regular_file(&self.root.join(rel), MAX_TEXT_FILE_BYTES) {
            Ok(text) => Some(text),
            Err(error) => {
                self.error(format!("Cannot read {rel}: {}", read_failure(&error)));
                None
            }
        }
    }

    /// Validates that a JSON manifest exists, parses to an object and has the
    /// required keys. Returns the document only when it is an object.
    fn validate_json(&mut self, rel: &str, required_keys: &[&str]) -> Option<Json> {
        if !self.root.join(rel).exists() {
            self.error(format!("Missing file: {rel}"));
            return None;
        }
        let text = self.read_text(rel)?;
        let data = match parse_json(&text) {
            Ok(data) => data,
            Err(error) => {
                self.error(format!("Invalid JSON in {rel}: {error}"));
                return None;
            }
        };
        if !matches!(data, Json::Object(_)) {
            for key in required_keys {
                self.error(format!("Missing key '{key}' in {rel}"));
            }
            self.error(format!("Top-level value must be a JSON object in {rel}"));
            return None;
        }
        for key in required_keys {
            if data.get(key).is_none() {
                self.error(format!("Missing key '{key}' in {rel}"));
            }
        }
        Some(data)
    }

    fn validate_marketplace(&mut self, rel: &str, label: &str) {
        self.progress(format!("Validating {label} marketplace: {rel}"));
        let Some(data) = self.validate_json(rel, &["name", "plugins"]) else {
            return;
        };
        let plugins = match data.get("plugins") {
            None => return,
            Some(Json::Array(plugins)) => plugins,
            Some(_) => {
                self.error(format!("'plugins' must be an array in {rel}"));
                return;
            }
        };
        let repository_root =
            fs::canonicalize(self.root).unwrap_or_else(|_| self.root.to_path_buf());
        for plugin in plugins {
            let Some(name) = plugin.get("name") else {
                self.error(format!("Plugin missing 'name' in {rel}"));
                continue;
            };
            let name = display_value(name);
            // The source is resolved from the repository root, not from the
            // manifest location. A `{ "path": ... }` object carries the path.
            let source = match plugin.get("source") {
                Some(object) if matches!(object, Json::Object(_)) => object.get("path"),
                other => other,
            };
            match source {
                None => {}
                Some(value) if is_falsy(value) => {}
                Some(Json::String(path)) => match check_inside(&repository_root, path) {
                    PathCheck::Escapes => self.error(format!(
                        "Plugin source '{path}' must stay inside the repository for '{name}'"
                    )),
                    PathCheck::Missing => self.error(format!(
                        "Plugin source '{path}' does not exist for '{name}'"
                    )),
                    PathCheck::Inside => {
                        if !self.root.join(path).is_dir() {
                            self.error(format!(
                                "Plugin source '{path}' does not exist for '{name}'"
                            ));
                        }
                    }
                },
                Some(_) => self.error(format!(
                    "Plugin source for '{name}' must be a string in {rel}"
                )),
            }
        }
    }

    fn validate_plugin(&mut self, name: &str) {
        let dir = format!("plugins/{name}");
        self.progress(format!("Validating plugin: {name}"));

        let claude = self.validate_json(&format!("{dir}/.claude-plugin/plugin.json"), &["name"]);
        self.validate_json(
            &format!("{dir}/.codex-plugin/plugin.json"),
            &["name", "version", "description", "author", "interface"],
        );
        self.check_cursor(name, &dir);
        self.check_devin(name, &dir, claude.as_ref());
        self.check_mcp(&dir);
        self.check_plugin_skills(&dir);
    }

    /// The Cursor manifest is optional; only its name is required and it must
    /// equal the plugin directory name.
    fn check_cursor(&mut self, name: &str, dir: &str) {
        let rel = format!("{dir}/.cursor-plugin/plugin.json");
        if !self.root.join(&rel).exists() {
            return;
        }
        let Some(cursor) = self.validate_json(&rel, &["name"]) else {
            return;
        };
        if let Some(value) = cursor.get("name") {
            if !is_falsy(value) && value.as_str() != Some(name) {
                self.error(format!(
                    "Name '{}' does not match directory '{name}' in {rel}",
                    display_value(value)
                ));
            }
        }
    }

    /// The Devin manifest is optional. Devin reads it in preference to the
    /// Claude manifest, so its metadata must match and the paths it declares
    /// must stay inside the plugin and exist.
    fn check_devin(&mut self, name: &str, dir: &str, claude: Option<&Json>) {
        let rel = format!("{dir}/.devin-plugin/plugin.json");
        if !self.root.join(&rel).exists() {
            return;
        }
        let Some(devin) = self.validate_json(&rel, &["name"]) else {
            return;
        };
        match devin.get("name") {
            Some(Json::String(devin_name)) if devin_name.is_empty() => {
                self.error(format!("'name' must be a non-empty string in {rel}"));
            }
            Some(Json::String(devin_name)) if devin_name != name => {
                self.error(format!(
                    "Name '{devin_name}' does not match directory '{name}' in {rel}"
                ));
            }
            Some(Json::String(_)) | None => {}
            Some(_) => self.error(format!("'name' must be a non-empty string in {rel}")),
        }
        if let Some(claude) = claude {
            let claude_rel = format!("{dir}/.claude-plugin/plugin.json");
            for key in ["name", "version", "description"] {
                if !py_eq_opt(devin.get(key), claude.get(key)) {
                    let detail = if key == "description" {
                        String::new()
                    } else {
                        format!(
                            ": {} != {}",
                            py_repr_opt(devin.get(key)),
                            py_repr_opt(claude.get(key))
                        )
                    };
                    self.error(format!(
                        "'{key}' in {rel} does not match {claude_rel}{detail}"
                    ));
                }
            }
        }
        self.check_devin_paths(dir, &devin, &rel);
    }

    /// Devin rejects the whole plugin when a declared path escapes the plugin
    /// root, so declared `skills` / `mcpServers` paths must stay inside and
    /// exist.
    fn check_devin_paths(&mut self, dir: &str, devin: &Json, rel: &str) {
        let plugin_dir = self.root.join(dir);
        let plugin_root = fs::canonicalize(&plugin_dir).unwrap_or(plugin_dir);
        for key in ["skills", "mcpServers"] {
            let entries: Vec<&Json> = match devin.get(key) {
                Some(Json::Array(items)) => items.iter().collect(),
                Some(single) if single.as_str().is_some() => vec![single],
                _ => Vec::new(),
            };
            for entry in entries {
                let Json::String(path) = entry else {
                    self.error(format!(
                        "'{key}' entry {} is not a path string in {rel}",
                        py_repr(entry)
                    ));
                    continue;
                };
                match check_inside(&plugin_root, path) {
                    PathCheck::Escapes => self.error(format!(
                        "'{key}' path '{path}' must stay inside the plugin directory in {rel}"
                    )),
                    PathCheck::Missing => {
                        self.error(format!("'{key}' path '{path}' does not exist in {rel}"));
                    }
                    PathCheck::Inside => {}
                }
            }
        }
    }

    fn check_mcp(&mut self, dir: &str) {
        let rel = format!("{dir}/.mcp.json");
        if !self.root.join(&rel).exists() {
            return;
        }
        let Some(data) = self.validate_json(&rel, &["mcpServers"]) else {
            return;
        };
        match data.get("mcpServers") {
            None => {}
            Some(Json::Object(servers)) => {
                for (server_name, server) in servers {
                    self.check_mcp_server(server_name, server, &rel);
                }
            }
            Some(_) => self.error(format!("'mcpServers' must be an object in {rel}")),
        }
    }

    fn check_mcp_server(&mut self, name: &str, server: &Json, rel: &str) {
        if !matches!(server, Json::Object(_)) {
            self.error(format!("MCP server '{name}' must be an object in {rel}"));
            return;
        }
        // An absent `type` means stdio; a non-string `type` selects no check.
        let kind = match server.get("type") {
            None => Some("stdio"),
            Some(Json::String(kind)) => Some(kind.as_str()),
            Some(_) => None,
        };
        if kind == Some("stdio") && server.get("command").is_none() {
            self.error(format!(
                "MCP server '{name}' is stdio but missing 'command'"
            ));
        } else if kind == Some("http") && server.get("url").is_none() {
            self.error(format!("MCP server '{name}' is http but missing 'url'"));
        }
        // Not all MCP clients expand ${VAR} / ${VAR:-default}: an unresolved
        // placeholder is passed through literally and breaks HTTP client
        // construction before any handshake.
        if let Some(Json::String(url)) = server.get("url") {
            if url.contains("${") {
                self.error(format!(
                    "MCP server '{name}' uses unsupported environment interpolation in URL: {url}"
                ));
            }
        }
    }

    fn check_plugin_skills(&mut self, dir: &str) {
        let skills = format!("{dir}/skills");
        if !self.root.join(&skills).is_dir() {
            return;
        }
        for child in self.list_dir(&skills) {
            let skill_dir = self.root.join(&skills).join(&child);
            if skill_dir.is_dir() && skill_dir.join("SKILL.md").exists() {
                self.validate_skill_frontmatter(&format!("{skills}/{child}/SKILL.md"), &child);
            }
        }
    }

    /// Every `SKILL.md` under `skills/`, found recursively without following
    /// symbolic links to directories, in component-wise path order.
    fn validate_top_level_skills(&mut self) {
        if !self.root.join("skills").is_dir() {
            return;
        }
        let mut found: Vec<Vec<String>> = Vec::new();
        let mut problems: Vec<String> = Vec::new();
        collect_skill_files(
            &self.root.join("skills"),
            "skills",
            &mut Vec::new(),
            &mut found,
            &mut problems,
        );
        for problem in problems {
            self.error(problem);
        }
        found.sort();
        for parts in found {
            let parent = &parts[..parts.len() - 1];
            let skill_dir = if parent.is_empty() {
                "skills".to_string()
            } else {
                format!("skills/{}", parent.join("/"))
            };
            let dir_name = parent.last().map_or("skills", String::as_str).to_string();
            self.progress(format!("Validating skill: {skill_dir}"));
            self.validate_skill_frontmatter(&format!("{skill_dir}/SKILL.md"), &dir_name);
        }
    }

    /// Validates that `SKILL.md` has simple `key: value` frontmatter with a
    /// kebab-case `name` equal to its directory and a real `description`.
    fn validate_skill_frontmatter(&mut self, rel: &str, dir_name: &str) {
        let Some(raw) = self.read_text(rel) else {
            return;
        };
        let text = universal_newlines(&raw);
        if !text.starts_with("---\n") {
            self.error(format!("Missing YAML frontmatter in {rel}"));
            return;
        }
        // The closing marker is searched from offset 4, so `---\n---\n`
        // (empty frontmatter) is unterminated, exactly as before.
        let Some(end) = text[4..].find("\n---\n").map(|index| index + 4) else {
            self.error(format!("Unterminated frontmatter in {rel}"));
            return;
        };
        let fields = parse_simple_frontmatter(&text[4..end]);

        let name = fields.get("name").map_or("", String::as_str);
        if name.is_empty() {
            self.error(format!("Missing 'name' in frontmatter: {rel}"));
        } else if !is_kebab(name) {
            self.error(format!("Name '{name}' is not kebab-case in {rel}"));
        } else if name.chars().count() > 64 {
            self.error(format!("Name exceeds 64 chars in {rel}"));
        } else if name != dir_name {
            self.error(format!(
                "Name '{name}' does not match directory '{dir_name}' in {rel}"
            ));
        }

        let description = fields.get("description").map_or("", String::as_str);
        if description.is_empty() {
            self.error(format!("Missing 'description' in frontmatter: {rel}"));
        } else if description.chars().count() < 20 {
            self.error(format!("Description too short (<20 chars) in {rel}"));
        }
    }
}

fn collect_skill_files(
    dir: &Path,
    dir_label: &str,
    prefix: &mut Vec<String>,
    found: &mut Vec<Vec<String>>,
    problems: &mut Vec<String>,
) {
    if prefix.len() >= MAX_WALK_DEPTH {
        problems.push(format!("Directory nesting too deep under {dir_label}"));
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            problems.push(format!("Cannot list {dir_label}: {error}"));
            return;
        }
    };
    let mut children: Vec<(String, bool)> = entries
        .filter_map(Result::ok)
        .map(|entry| {
            // `file_type` does not follow links: a symlinked directory is
            // never descended into.
            let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
            (entry.file_name().to_string_lossy().into_owned(), is_dir)
        })
        .collect();
    children.sort();
    for (name, is_dir) in children {
        if name == "SKILL.md" {
            let mut parts = prefix.clone();
            parts.push(name.clone());
            found.push(parts);
        }
        if is_dir {
            prefix.push(name.clone());
            collect_skill_files(
                &dir.join(&name),
                &format!("{dir_label}/{name}"),
                prefix,
                found,
                problems,
            );
            prefix.pop();
        }
    }
}

enum PathCheck {
    Inside,
    Escapes,
    Missing,
}

/// Whether the path declared in a manifest stays inside `plugin_root` (which
/// must already be canonical) and exists. Absolute paths, `..` components and
/// symbolic links that lead out of the plugin all escape.
fn check_inside(plugin_root: &Path, declared: &str) -> PathCheck {
    let relative = Path::new(declared);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return PathCheck::Escapes;
    }
    let target = resolve_lenient(&plugin_root.join(relative));
    if !target.starts_with(plugin_root) {
        PathCheck::Escapes
    } else if target.exists() {
        PathCheck::Inside
    } else {
        PathCheck::Missing
    }
}

/// Canonicalizes the longest existing prefix of `path` and appends the rest,
/// so a path that does not exist yet still has its existing symlinks resolved.
pub(crate) fn resolve_lenient(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut tail: Vec<OsString> = Vec::new();
    loop {
        if let Ok(real) = fs::canonicalize(&existing) {
            let mut resolved = real;
            for part in tail.iter().rev() {
                resolved.push(part);
            }
            return resolved;
        }
        let name = existing.file_name().map(ToOwned::to_owned);
        let parent = existing.parent().map(Path::to_path_buf);
        match (name, parent) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

pub(crate) fn read_failure(error: &PathError) -> String {
    match error {
        PathError::Invalid(reason) => reason.clone(),
        PathError::NotFound(_) => "file does not exist".to_string(),
        PathError::Symlink(_) => "symbolic links are not followed".to_string(),
        PathError::NotRegularFile(_) => "not a regular file".to_string(),
        PathError::TooLarge { limit, .. } => format!("larger than {limit} bytes"),
        PathError::NotUtf8(_) => "not valid UTF-8".to_string(),
        PathError::Changed(_) => "changed while it was being opened".to_string(),
        PathError::Io(error, _) => error.to_string(),
    }
}

/// Mirrors Python's text-mode newline translation: `\r\n` and lone `\r`
/// become `\n`.
pub(crate) fn universal_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Python's `str.strip()` whitespace: Unicode whitespace plus the ASCII
/// separators `\x1c`-`\x1f`.
fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

pub(crate) fn py_strip(text: &str) -> &str {
    text.trim_matches(is_py_space)
}

/// Python's `str.splitlines()`: line boundaries are `\n`, `\r`, `\r\n`, `\v`,
/// `\f`, `\x1c`-`\x1e`, `\x85`, `\u{2028}` and `\u{2029}`; no empty trailing line.
fn py_splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        if matches!(
            c,
            '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}'
                ..='\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        ) {
            lines.push(&text[start..index]);
            let mut next = index + c.len_utf8();
            if c == '\r' && matches!(chars.peek(), Some((_, '\n'))) {
                chars.next();
                next += 1;
            }
            start = next;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// The deliberately simple `key: value` frontmatter reader of the Python
/// validator: top-level `key: value` lines (value stripped of whitespace, then
/// of `"` and `'` characters, block-scalar indicators dropped) and lines that
/// start with two spaces continuing the previous non-empty key.
fn parse_simple_frontmatter(block: &str) -> HashMap<String, String> {
    let mut fields: HashMap<String, String> = HashMap::new();
    let mut current: Option<String> = None;
    for line in py_splitlines(block) {
        let pair = if line.starts_with(' ') {
            None
        } else {
            line.split_once(':')
        };
        if let Some((key, value)) = pair {
            let mut value = py_strip(value).trim_matches('"').trim_matches('\'');
            if matches!(value, ">" | "|" | ">-" | "|-") {
                value = "";
            }
            let key = py_strip(key).to_string();
            fields.insert(key.clone(), value.to_string());
            current = if key.is_empty() { None } else { Some(key) };
        } else if line.starts_with("  ") {
            if let Some(key) = &current {
                let existing = fields.get(key).map_or("", String::as_str);
                let joined = format!("{existing} {}", py_strip(line));
                let joined = py_strip(&joined).to_string();
                fields.insert(key.clone(), joined);
            }
        }
    }
    fields
}

/// `^[a-z][a-z0-9]+(-[a-z0-9]+)*$`
fn is_kebab(name: &str) -> bool {
    fn kebab_char(c: char) -> bool {
        c.is_ascii_lowercase() || c.is_ascii_digit()
    }
    let mut segments = name.split('-');
    let Some(first) = segments.next() else {
        return false;
    };
    let mut chars = first.chars();
    let Some(head) = chars.next() else {
        return false;
    };
    let rest = chars.as_str();
    head.is_ascii_lowercase()
        && !rest.is_empty()
        && rest.chars().all(kebab_char)
        && segments.all(|segment| !segment.is_empty() && segment.chars().all(kebab_char))
}

/// Python truthiness of a JSON value (`float` zero is not special-cased).
pub(crate) fn is_falsy(value: &Json) -> bool {
    match value {
        Json::Null | Json::Bool(false) | Json::Int(0) => true,
        Json::String(text) => text.is_empty(),
        Json::Array(items) => items.is_empty(),
        Json::Object(members) => members.is_empty(),
        Json::Bool(true) | Json::Int(_) | Json::Float(_) => false,
    }
}

/// Strings as themselves, anything else as compact JSON.
fn display_value(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        other => other.to_compact_string(),
    }
}

pub(crate) fn py_repr_opt(value: Option<&Json>) -> String {
    value.map_or_else(|| "None".to_string(), py_repr)
}

/// Python `==` between the values `dict.get` returns: an absent key is `None`,
/// so it equals a JSON `null`.
fn py_eq_opt(left: Option<&Json>, right: Option<&Json>) -> bool {
    py_eq(left.unwrap_or(&Json::Null), right.unwrap_or(&Json::Null))
}

/// Python `==` on parsed JSON, recursively: objects compare as dicts (same
/// keys, member order irrelevant; an absent nested key is not `None`), `True`
/// and `False` equal `1` and `0`, and integers equal floats of the same value.
fn py_eq(left: &Json, right: &Json) -> bool {
    match (left, right) {
        (Json::Null, Json::Null) => true,
        (Json::String(a), Json::String(b)) => a == b,
        (Json::Array(a), Json::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| py_eq(x, y))
        }
        (Json::Object(a), Json::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, x)| right.get(key).is_some_and(|y| py_eq(x, y)))
        }
        _ => match (py_number(left), py_number(right)) {
            (Some(a), Some(b)) => numbers_equal(a, b),
            _ => false,
        },
    }
}

/// A Python number: `bool` is an `int` subclass.
#[derive(Clone, Copy)]
enum PyNumber {
    Int(i64),
    Float(f64),
}

fn py_number(value: &Json) -> Option<PyNumber> {
    match value {
        Json::Bool(flag) => Some(PyNumber::Int(i64::from(*flag))),
        Json::Int(number) => Some(PyNumber::Int(*number)),
        Json::Float(number) => Some(PyNumber::Float(*number)),
        _ => None,
    }
}

/// Exact numeric equality, as Python compares an `int` with a `float`. Exact
/// float comparison and the range-checked `f64` -> `i64` conversion are the
/// intent here.
#[allow(clippy::float_cmp, clippy::cast_possible_truncation)]
fn numbers_equal(left: PyNumber, right: PyNumber) -> bool {
    match (left, right) {
        (PyNumber::Int(a), PyNumber::Int(b)) => a == b,
        (PyNumber::Float(a), PyNumber::Float(b)) => a == b,
        (PyNumber::Int(int), PyNumber::Float(float))
        | (PyNumber::Float(float), PyNumber::Int(int)) => {
            // The i64 range is [-2^63, 2^63); both bounds are exact in f64.
            float.fract() == 0.0
                && (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&float)
                && float as i64 == int
        }
    }
}

/// Python-style `repr()` of a JSON value, used in mismatch messages.
pub(crate) fn py_repr(value: &Json) -> String {
    match value {
        Json::Null => "None".to_string(),
        Json::Bool(true) => "True".to_string(),
        Json::Bool(false) => "False".to_string(),
        Json::Int(number) => number.to_string(),
        Json::Float(number) => number.to_string(),
        Json::String(text) => py_repr_str(text),
        Json::Array(items) => {
            let inner: Vec<String> = items.iter().map(py_repr).collect();
            format!("[{}]", inner.join(", "))
        }
        Json::Object(members) => {
            let inner: Vec<String> = members
                .iter()
                .map(|(key, member)| format!("{}: {}", py_repr_str(key), py_repr(member)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

pub(crate) fn py_repr_str(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if u32::from(c) < 0x20 || u32::from(c) == 0x7f => {
                let _ = write!(out, "\\x{:02x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

#[cfg(test)]
mod tests;
