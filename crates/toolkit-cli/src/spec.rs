//! `toolkit spec validate`: the Rust replacement for `tools/validate_spec.py`
//! (#4376).
//!
//! The command checks, offline and read-only, that every plugin under
//! `ROOT/plugins` conforms to Agent Plugins v1.0.0:
//!
//! 1. no symbolic links anywhere under `plugins/`;
//! 2. every `plugin.json` and `mcp.json` validates against the vendored
//!    schemas in `ROOT/tools/schemas/1.0.0` (evaluated offline by the
//!    `engine` submodule on the `jsonschema` crate);
//! 3. the rules the schemas cannot express: path containment (spec section
//!    4.1), placeholder restrictions (9.2) and MCP URL and header rules
//!    (7.2.1);
//! 4. every skill in `plugins/*/skills/*/SKILL.md` passes the Agent Skills
//!    frontmatter, length and naming rules.
//!
//! Findings are collected in order as a [`Report`] (shared with
//! `manifests validate`); the handler prints `Validating ...` progress lines
//! on stdout and `  ERROR: <message>` lines on stderr and fails with exit
//! status 1 when anything was found. Rule parity and the deliberate
//! differences from the Python tool are recorded in
//! `docs/migration/spec-validator.md`.

use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

use toolkit_core::{
    parse_json, parse_yaml, read_regular_file, Json, ToolkitError, MAX_TEXT_FILE_BYTES,
};

use crate::manifests::{
    is_falsy, py_repr, py_repr_opt, py_repr_str, py_strip, read_failure, resolve_lenient,
    universal_newlines, Event, Report,
};
use crate::{Invocation, Output};

use unicode_normalization::UnicodeNormalization;

pub(crate) mod engine;
#[cfg(test)]
mod tests;

const SCHEMA_DIR: &str = "tools/schemas/1.0.0";
const PLUGIN_SCHEMA_ID: &str = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";
const MCP_SCHEMA_ID: &str = "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json";

const ALLOWED_SKILL_FIELDS: &[&str] = &[
    "name",
    "description",
    "license",
    "allowed-tools",
    "metadata",
    "compatibility",
];
const MAX_NAME: usize = 64;
const MAX_DESCRIPTION: usize = 1024;
const MAX_COMPATIBILITY: usize = 500;

const RESERVED_PLACEHOLDERS: &[&str] = &["PLUGIN_ROOT", "PLUGIN_DATA"];

/// Deepest `plugins/` nesting the symlink scan follows.
const MAX_WALK_DEPTH: usize = 64;

/// Validates the repository at `root`.
///
/// # Errors
///
/// Returns a runtime error when `root` is missing or not a directory, when a
/// vendored schema cannot be read, parsed or evaluated, or when
/// `root/plugins` cannot be listed. Validation findings are not errors here:
/// they are in the returned [`Report`].
pub fn validate_tree(root: &Path) -> Result<Report, ToolkitError> {
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
    let plugin_schema = load_schema(root, "plugin.schema.json")?;
    let mcp_schema = load_schema(root, "mcp.schema.json")?;
    let mut validator = SpecValidator {
        root,
        plugin_schema,
        mcp_schema,
        report: Report::default(),
    };

    validator.reject_symlinks();

    let plugins_dir = root.join("plugins");
    let names = fs::read_dir(&plugins_dir)
        .map(|entries| names_of(entries.filter_map(Result::ok)))
        .map_err(|error| {
            ToolkitError::runtime(
                "plugins_not_found",
                format!("cannot list {}: {error}", plugins_dir.display()),
            )
        })?;
    for name in &names {
        if plugins_dir.join(name).is_dir() {
            // One malformed plugin must not stop the others from being checked.
            validator.validate_plugin_dir(name)?;
        }
    }

    validator.progress("Validating skills".to_string());
    for (plugin, skill) in find_skills(&plugins_dir, &names) {
        validator.validate_skill(&plugin, &skill);
    }
    Ok(validator.report)
}

/// Handler for `toolkit spec validate [ROOT]`.
///
/// # Errors
///
/// Fails with a runtime error for an unusable root, schema or `plugins`
/// directory and with `validation_failed` (exit status 1) when the report
/// contains errors.
pub fn validate_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let root = PathBuf::from(invocation.positional(0).unwrap_or("."));
    let report = validate_tree(&root)?;
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
            format!("Spec validation failed with {count} error(s)."),
        ));
    }
    output.line("")?;
    output.line("All Agent Plugins v1.0.0 checks passed.")
}

fn load_schema(root: &Path, name: &str) -> Result<engine::Schema, ToolkitError> {
    let shown = format!("{SCHEMA_DIR}/{name}");
    let text = read_regular_file(&root.join(SCHEMA_DIR).join(name), MAX_TEXT_FILE_BYTES).map_err(
        |error| {
            ToolkitError::runtime(
                "schema_unreadable",
                format!("cannot read schema {shown}: {}", read_failure(&error)),
            )
        },
    )?;
    let schema = parse_json(&text).map_err(|error| {
        ToolkitError::runtime("schema_invalid", format!("invalid schema {shown}: {error}"))
    })?;
    engine::compile(schema).map_err(|reason| {
        ToolkitError::runtime(
            "schema_unsupported",
            format!("unsupported schema {shown}: {reason}"),
        )
    })
}

/// Sorted file names of directory entries (lossy for non-UTF-8 names).
fn names_of(entries: impl Iterator<Item = fs::DirEntry>) -> Vec<String> {
    let mut names: Vec<String> = entries
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn sorted_names(dir: &Path) -> Vec<String> {
    fs::read_dir(dir).map_or_else(
        |_| Vec::new(),
        |entries| names_of(entries.filter_map(Result::ok)),
    )
}

/// `plugins/*/skills/*/SKILL.md` as `(plugin, skill)` pairs in sorted order.
/// Like the Python glob, unreadable directories are skipped silently and
/// symbolic links to directories are followed.
fn find_skills(plugins_dir: &Path, plugin_names: &[String]) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for plugin in plugin_names {
        let skills_dir = plugins_dir.join(plugin).join("skills");
        if !plugins_dir.join(plugin).is_dir() || !skills_dir.is_dir() {
            continue;
        }
        for skill in sorted_names(&skills_dir) {
            let candidate = skills_dir.join(&skill);
            if candidate.is_dir() && candidate.join("SKILL.md").exists() {
                found.push((plugin.clone(), skill));
            }
        }
    }
    found
}

#[derive(Clone, Copy)]
enum SchemaKind {
    Plugin,
    Mcp,
}

struct SpecValidator<'a> {
    root: &'a Path,
    plugin_schema: engine::Schema,
    mcp_schema: engine::Schema,
    report: Report,
}

impl SpecValidator<'_> {
    fn progress(&mut self, text: String) {
        self.report.events.push(Event::Progress(text));
    }

    fn error(&mut self, text: String) {
        self.report.events.push(Event::Error(text));
    }

    /// Spec 4.1 and the Python tool's check 1: no symlinks under `plugins/`.
    fn reject_symlinks(&mut self) {
        let plugins = self.root.join("plugins");
        if !plugins.is_dir() {
            return;
        }
        let mut links: Vec<Vec<String>> = Vec::new();
        let mut problems: Vec<String> = Vec::new();
        scan_links(
            &plugins,
            &["plugins".to_string()],
            0,
            &mut links,
            &mut problems,
        );
        links.sort();
        for link in links {
            self.error(format!(
                "{}: symlinks are not allowed under plugins/",
                link.join("/")
            ));
        }
        for problem in problems {
            self.error(problem);
        }
    }

    /// Reads and parses a JSON file; problems become errors labelled `rel`.
    fn read_json(&mut self, rel: &str) -> Option<Json> {
        let text = match read_regular_file(&self.root.join(rel), MAX_TEXT_FILE_BYTES) {
            Ok(text) => text,
            Err(error) => {
                self.error(format!("{rel}: cannot read: {}", read_failure(&error)));
                return None;
            }
        };
        match parse_json(&text) {
            Ok(document) => Some(document),
            Err(error) => {
                self.error(format!("{rel}: invalid JSON: {error}"));
                None
            }
        }
    }

    /// Reports every schema violation of `document`. A schema that cannot be
    /// evaluated (for example a regular expression that exhausts its limit)
    /// is a runtime error, never a finding and never a pass.
    fn schema_errors(
        &mut self,
        kind: SchemaKind,
        document: &Json,
        label: &str,
    ) -> Result<(), ToolkitError> {
        let schema = match kind {
            SchemaKind::Plugin => &self.plugin_schema,
            SchemaKind::Mcp => &self.mcp_schema,
        };
        let issues = schema.validate(document).map_err(|reason| {
            ToolkitError::runtime(
                "schema_evaluation_failed",
                format!("schema evaluation failed for {label}: {reason}"),
            )
        })?;
        for issue in issues {
            self.error(format!(
                "{label}: {} (at {})",
                issue.message,
                issue.path_text()
            ));
        }
        Ok(())
    }

    fn validate_plugin_dir(&mut self, name: &str) -> Result<(), ToolkitError> {
        let rel = format!("plugins/{name}");
        let plugin_dir = self.root.join("plugins").join(name);
        self.progress(format!("Validating plugin: {rel}"));

        if !plugin_dir.join("plugin.json").exists() {
            self.error(format!(
                "{rel}: missing required root plugin.json (spec §5.1)"
            ));
            return Ok(());
        }
        let manifest_rel = format!("{rel}/plugin.json");
        let Some(manifest) = self.read_json(&manifest_rel) else {
            return Ok(());
        };
        self.schema_errors(SchemaKind::Plugin, &manifest, &manifest_rel)?;
        if !matches!(manifest, Json::Object(_)) {
            // The schema already reported the wrong top-level type; the
            // member checks below need an object.
            return Ok(());
        }

        if manifest.get("$schema").and_then(Json::as_str) != Some(PLUGIN_SCHEMA_ID) {
            self.error(format!(
                "{manifest_rel}: $schema must be {PLUGIN_SCHEMA_ID}"
            ));
        }
        if manifest.get("name").and_then(Json::as_str) != Some(name) {
            self.error(format!(
                "{manifest_rel}: name {} != directory {}",
                py_repr_opt(manifest.get("name")),
                py_repr_str(name)
            ));
        }
        self.check_extensions(&manifest, &manifest_rel, &plugin_dir);

        let mcp_rel = format!("{rel}/mcp.json");
        if plugin_dir.join("mcp.json").exists() {
            if let Some(mcp) = self.read_json(&mcp_rel) {
                self.schema_errors(SchemaKind::Mcp, &mcp, &mcp_rel)?;
                if matches!(mcp, Json::Object(_)) {
                    if mcp.get("$schema").and_then(Json::as_str) != Some(MCP_SCHEMA_ID) {
                        self.error(format!("{mcp_rel}: $schema must be {MCP_SCHEMA_ID}"));
                    }
                    if let Some(Json::Object(servers)) = mcp.get("mcpServers") {
                        for (server_name, server) in servers {
                            self.validate_server(server_name, server, &mcp_rel, &plugin_dir);
                        }
                    }
                }
            }
        }

        let skills_dir = plugin_dir.join("skills");
        if skills_dir.exists() && !skills_dir.is_dir() {
            self.error(format!(
                "{rel}: skills exists but is not a directory (spec §6.2)"
            ));
        }
        Ok(())
    }

    /// Extension namespaces must be reverse-domain; every string leaf that can
    /// be resolved against the plugin root must stay inside it and exist
    /// (spec section 8).
    fn check_extensions(&mut self, manifest: &Json, manifest_rel: &str, plugin_dir: &Path) {
        let members: &[(String, Json)] = match manifest.get("extensions") {
            None | Some(Json::Null) => &[],
            Some(Json::Object(members)) => members,
            Some(_) => {
                self.error(format!(
                    "{manifest_rel}: extensions must be an object (spec §8.1)"
                ));
                &[]
            }
        };
        for (namespace, value) in members {
            if !namespace.contains('.') {
                self.error(format!(
                    "{manifest_rel}: extension namespace {} is not reverse-domain (spec §8)",
                    py_repr_str(namespace)
                ));
            }
            // Every string leaf is a candidate path, at any nesting depth:
            // a namespace owner can nest pointers arbitrarily, so type is the
            // only reliable filter.
            let mut leaves = Vec::new();
            collect_strings(
                value,
                &format!("{manifest_rel} extensions.{namespace}"),
                &mut leaves,
            );
            for (label, text) in leaves {
                if !looks_like_path(text) {
                    continue;
                }
                self.check_containment(text, &label, plugin_dir);
                if let Some(relative) = text.strip_prefix("./") {
                    if !plugin_dir.join(relative).exists() {
                        self.error(format!("{label}: points at missing {text}"));
                    }
                }
            }
        }
    }

    /// Spec 4.1: a plugin-relative path is `./`-prefixed and, once the
    /// filesystem resolves it, stays inside the resolved plugin root.
    ///
    /// Resolution matters: a lexical check passes a path that reaches outside
    /// the root through a symlink.
    fn check_containment(&mut self, rel_path: &str, label: &str, plugin_dir: &Path) {
        if rel_path.contains('\\') {
            self.error(format!(
                "{label}: path must use '/' separators (got {})",
                py_repr_str(rel_path)
            ));
            return;
        }
        let Some(relative) = rel_path.strip_prefix("./") else {
            self.error(format!(
                "{label}: plugin-relative path must begin with './' (got {})",
                py_repr_str(rel_path)
            ));
            return;
        };
        if rel_path.split('/').any(|part| part == "..") {
            self.error(format!(
                "{label}: path escapes the plugin root ({})",
                py_repr_str(rel_path)
            ));
            return;
        }
        let root = fs::canonicalize(plugin_dir).unwrap_or_else(|_| plugin_dir.to_path_buf());
        let target = resolve_lenient(&root.join(relative));
        if !target.starts_with(&root) {
            self.error(format!(
                "{label}: path resolves outside the plugin root ({} -> {})",
                py_repr_str(rel_path),
                target.display()
            ));
        }
    }

    /// Containment for MCP runtime values, which become a subprocess. A
    /// `${PLUGIN_ROOT}` or `${PLUGIN_DATA}` prefix stands in for the root, so
    /// strip it and check the remainder for traversal.
    fn check_runtime_path(&mut self, value: &str, label: &str, plugin_dir: &Path) {
        for placeholder in ["${PLUGIN_ROOT}", "${PLUGIN_DATA}"] {
            if let Some(rest) = value.strip_prefix(placeholder) {
                let remainder = rest.trim_start_matches('/');
                let relative = if remainder.is_empty() {
                    "./".to_string()
                } else {
                    format!("./{remainder}")
                };
                self.check_containment(&relative, label, plugin_dir);
                return;
            }
        }
        self.check_containment(value, label, plugin_dir);
    }

    /// Spec 7.2.1 and 9.2 rules the JSON Schema cannot express.
    fn validate_server(&mut self, name: &str, server: &Json, label: &str, plugin_dir: &Path) {
        let who = format!("{label}: server {}", py_repr_str(name));
        let context = format!("{label} server {}", py_repr_str(name));
        if !matches!(server, Json::Object(_)) {
            self.error(format!("{who} must be an object"));
            return;
        }
        match server.get("type").and_then(Json::as_str) {
            Some("stdio") => self.check_stdio(server, &who, &context, plugin_dir),
            Some("streamable-http" | "sse") => self.check_http(server, &who),
            _ => {}
        }
    }

    fn check_stdio(&mut self, server: &Json, who: &str, context: &str, plugin_dir: &Path) {
        let command = if let Some(Json::String(command)) = server.get("command") {
            command.as_str()
        } else {
            self.error(format!("{who} command must be a string"));
            ""
        };
        if !find_placeholders(command).is_empty() {
            self.error(format!(
                "{who} command must not use placeholders (spec §7.2.1)"
            ));
        }
        if !command.starts_with("./") && command.contains('/') {
            self.error(format!("{who} command must be a bare name or './' path"));
        }
        if command.starts_with("./") {
            self.check_containment(command, &format!("{context} command"), plugin_dir);
        }

        let env: &[(String, Json)] = match server.get("env") {
            None | Some(Json::Null) => &[],
            Some(Json::Object(members)) => members,
            Some(_) => {
                self.error(format!("{who} env must be an object"));
                &[]
            }
        };
        for key in RESERVED_PLACEHOLDERS {
            if env.iter().any(|(name, _)| name == key) {
                self.error(format!("{who} env must not set {key} (spec §9.2)"));
            }
        }

        for field in ["args", "cwd"] {
            let values: Vec<&Json> = match server.get(field) {
                Some(Json::Array(items)) => items.iter().collect(),
                Some(value) if !is_falsy(value) => vec![value],
                _ => Vec::new(),
            };
            for value in values {
                let Json::String(text) = value else {
                    self.error(format!("{who} {field} entries must be strings"));
                    continue;
                };
                for placeholder in find_placeholders(text) {
                    if !RESERVED_PLACEHOLDERS.contains(&placeholder) {
                        self.error(format!(
                            "{who} {field} uses unsupported placeholder ${{{placeholder}}} (spec §9.2)"
                        ));
                    }
                }
                // These values become a subprocess path, so containment is
                // enforced on anything resolvable against the plugin root.
                if looks_like_path(text) {
                    self.check_runtime_path(text, &format!("{context} {field}"), plugin_dir);
                }
            }
        }
    }

    fn check_http(&mut self, server: &Json, who: &str) {
        let Some(url) = server.get("url").and_then(Json::as_str) else {
            self.error(format!("{who} url must be a string"));
            return;
        };
        if !find_placeholders(url).is_empty() {
            self.error(format!("{who} url must not use expansion (spec §7.2.1)"));
        }
        match split_url(url) {
            Ok(parts) => {
                if !parts.fragment.is_empty() || parts.has_credentials {
                    self.error(format!(
                        "{who} url must not contain user info or a fragment (spec §7.2.1)"
                    ));
                }
                let loopback = matches!(
                    parts.hostname.as_deref(),
                    Some("127.0.0.1" | "localhost" | "::1")
                );
                if parts.scheme != "https" && !loopback {
                    self.error(format!(
                        "{who} non-loopback url must use HTTPS (spec §7.2.1)"
                    ));
                }
            }
            Err(reason) => self.error(format!("{who} url is not a valid URL ({reason})")),
        }

        let headers: &[(String, Json)] = match server.get("headers") {
            None | Some(Json::Null) => &[],
            Some(Json::Object(members)) => members,
            Some(_) => {
                self.error(format!("{who} headers must be an object"));
                &[]
            }
        };
        let mut lowered: Vec<String> = headers
            .iter()
            .map(|(name, _)| name.to_lowercase())
            .collect();
        let total = lowered.len();
        lowered.sort();
        lowered.dedup();
        if lowered.len() != total {
            self.error(format!(
                "{who} has duplicate header names under different casing"
            ));
        }
        for (name, value) in headers {
            if let Json::String(text) = value {
                if !find_placeholders(text).is_empty() {
                    self.error(format!(
                        "{who} header {} must not use expansion (spec §7.2.1)",
                        py_repr_str(name)
                    ));
                }
            }
            if name.to_lowercase() == "authorization" {
                self.error(format!(
                    "{who} must not embed credentials in headers (spec §7.2.1)"
                ));
            }
        }
    }

    fn validate_skill(&mut self, plugin: &str, skill: &str) {
        let rel = format!("plugins/{plugin}/skills/{skill}/SKILL.md");
        let raw = match read_regular_file(&self.root.join(&rel), MAX_TEXT_FILE_BYTES) {
            // Python reads in text mode, which translates newlines.
            Ok(text) => universal_newlines(&text),
            Err(error) => {
                self.error(format!(
                    "{rel}: cannot read as UTF-8: {}",
                    read_failure(&error)
                ));
                return;
            }
        };
        let fields = match parse_frontmatter(&raw) {
            Frontmatter::Missing => {
                self.error(format!("{rel}: missing or unterminated YAML frontmatter"));
                return;
            }
            Frontmatter::Invalid(reason) => {
                self.error(format!("{rel}: invalid YAML frontmatter: {reason}"));
                return;
            }
            Frontmatter::Mapping(fields) => fields,
        };
        let field = |key: &str| {
            fields
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value)
        };

        let mut extra: Vec<&str> = fields
            .iter()
            .map(|(name, _)| name.as_str())
            .filter(|name| !ALLOWED_SKILL_FIELDS.contains(name))
            .collect();
        extra.sort_unstable();
        if !extra.is_empty() {
            self.error(format!(
                "{rel}: non-spec frontmatter field(s): {}",
                extra.join(", ")
            ));
        }

        let name = truthy_text(field("name"));
        if name.is_empty() {
            self.error(format!("{rel}: missing required 'name'"));
        } else {
            self.check_skill_name(&rel, &name, skill);
        }

        let description = truthy_text(field("description"));
        let description = py_strip(&description);
        if description.is_empty() {
            self.error(format!("{rel}: missing required 'description'"));
        } else if description.chars().count() > MAX_DESCRIPTION {
            self.error(format!(
                "{rel}: description is {} chars, over the {MAX_DESCRIPTION} limit",
                description.chars().count()
            ));
        }

        if truthy_text(field("compatibility")).chars().count() > MAX_COMPATIBILITY {
            self.error(format!(
                "{rel}: compatibility exceeds {MAX_COMPATIBILITY} chars"
            ));
        }

        if let Some(tools) = field("allowed-tools") {
            if !matches!(tools, Json::Null | Json::String(_)) {
                self.error(format!(
                    "{rel}: allowed-tools must be a space-separated string, not a sequence"
                ));
            }
        }
    }

    fn check_skill_name(&mut self, rel: &str, name: &str, directory: &str) {
        let normalized = nfkc(name);
        if normalized.chars().count() > MAX_NAME {
            self.error(format!("{rel}: name exceeds {MAX_NAME} chars"));
        }
        if normalized != normalized.to_lowercase() {
            self.error(format!("{rel}: name must be lowercase"));
        }
        if normalized.starts_with('-') || normalized.ends_with('-') {
            self.error(format!("{rel}: name must not start or end with a hyphen"));
        }
        if normalized.contains("--") {
            self.error(format!("{rel}: name must not contain consecutive hyphens"));
        }
        if !normalized.chars().all(|c| c.is_alphanumeric() || c == '-') {
            self.error(format!("{rel}: name has invalid characters"));
        }
        if normalized != nfkc(directory) {
            self.error(format!(
                "{rel}: name {} != directory {}",
                py_repr_str(name),
                py_repr_str(directory)
            ));
        }
    }
}

/// Collects symbolic links below `dir` (not following them), as relative
/// component lists, plus listing problems.
fn scan_links(
    dir: &Path,
    rel: &[String],
    depth: usize,
    links: &mut Vec<Vec<String>>,
    problems: &mut Vec<String>,
) {
    if depth > MAX_WALK_DEPTH {
        problems.push(format!("{}: directory nesting is too deep", rel.join("/")));
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            problems.push(format!("{}: cannot list directory: {error}", rel.join("/")));
            return;
        }
    };
    for entry in entries.filter_map(Result::ok) {
        let mut child = rel.to_vec();
        child.push(entry.file_name().to_string_lossy().into_owned());
        match entry.file_type() {
            Ok(kind) if kind.is_symlink() => links.push(child),
            Ok(kind) if kind.is_dir() => {
                scan_links(&entry.path(), &child, depth + 1, links, problems);
            }
            _ => {}
        }
    }
}

/// Every string leaf under `node` with a `prefix.key` / `prefix[index]` label.
fn collect_strings<'a>(node: &'a Json, prefix: &str, out: &mut Vec<(String, &'a str)>) {
    match node {
        Json::String(text) => out.push((prefix.to_string(), text.as_str())),
        Json::Object(members) => {
            for (key, value) in members {
                collect_strings(value, &format!("{prefix}.{key}"), out);
            }
        }
        Json::Array(items) => {
            for (index, value) in items.iter().enumerate() {
                collect_strings(value, &format!("{prefix}[{index}]"), out);
            }
        }
        _ => {}
    }
}

/// Values a client could resolve against the plugin root.
fn looks_like_path(value: &str) -> bool {
    value.starts_with("./")
        || value.starts_with("../")
        || value.starts_with('/')
        || value.starts_with('\\')
        || value.starts_with("${PLUGIN_ROOT}")
        || value.starts_with("${PLUGIN_DATA}")
        || value.contains('\\')
}

/// The contents of every `${...}` in `text`, like `re.findall` of
/// `\$\{([^}]*)\}`.
fn find_placeholders(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            break;
        };
        found.push(&after[..end]);
        rest = &after[end + 1..];
    }
    found
}

/// `str(value or "")`: empty for a missing or falsy value, otherwise the
/// Python text of the value.
fn truthy_text(value: Option<&Json>) -> String {
    match value {
        Some(value) if !is_falsy(value) => match value {
            Json::String(text) => text.clone(),
            other => py_repr(other),
        },
        _ => String::new(),
    }
}

/// Unicode NFKC normalization, as `unicodedata.normalize("NFKC", text)`:
/// canonical composition (`e` + U+0301 becomes U+00E9) and compatibility
/// expansion (U+FB00 becomes `ff`, fullwidth forms become ASCII). Names and
/// directory names are normalized with this before any check, so every length,
/// case, character and equality test sees the normalized text.
fn nfkc(text: &str) -> String {
    text.nfkc().collect()
}

#[derive(Debug)]
enum Frontmatter {
    Missing,
    Invalid(String),
    Mapping(Vec<(String, Json)>),
}

/// Reads the YAML frontmatter block: `---\n` first, closed by a `\n---\n`
/// found at or after offset 4 (an empty block is therefore unterminated).
fn parse_frontmatter(text: &str) -> Frontmatter {
    if !text.starts_with("---\n") {
        return Frontmatter::Missing;
    }
    let Some(end) = text[4..].find("\n---\n").map(|index| index + 4) else {
        return Frontmatter::Missing;
    };
    match parse_yaml(&text[4..end]) {
        Err(error) if error.line > 0 => Frontmatter::Invalid(format!(
            "{} (line {} of the file)",
            error.message,
            error.line + 1
        )),
        Err(error) => Frontmatter::Invalid(error.message),
        Ok(Json::Object(members)) => Frontmatter::Mapping(members),
        Ok(_) => Frontmatter::Invalid("frontmatter is not a mapping".to_string()),
    }
}

/// The URL parts the server rules look at, split as Python's `urlsplit`
/// does.
#[derive(Debug, PartialEq, Eq)]
struct UrlParts {
    scheme: String,
    fragment: String,
    has_credentials: bool,
    hostname: Option<String>,
}

/// `urllib.parse.urlsplit` plus the `hostname` and `username`/`password`
/// accessors the Python tool reads.
///
/// Like `urlsplit` it fails, with the same wording, on a bracket without its
/// partner, on a bracketed host that is not an IPv6 literal (a name or an IPv4
/// address in brackets, or a malformed `v...` `IPvFuture`), and on a netloc
/// whose NFKC form would contain a URL delimiter (for example U+FF0F, the
/// fullwidth solidus).
fn split_url(url: &str) -> Result<UrlParts, String> {
    // Python strips leading C0 control characters and spaces and removes
    // tabs and newlines anywhere before splitting.
    let cleaned: String = url
        .trim_start_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut rest: &str = &cleaned;

    let mut scheme = String::new();
    if let Some(colon) = rest.find(':') {
        let candidate = &rest[..colon];
        let valid = candidate
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
            && candidate
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
        if valid {
            scheme = candidate.to_ascii_lowercase();
            rest = &rest[colon + 1..];
        }
    }

    let mut netloc = "";
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = &after[..end];
        rest = &after[end..];
        if netloc.contains('[') != netloc.contains(']') {
            return Err("Invalid IPv6 URL".to_string());
        }
        if let Some((_, tail)) = netloc.split_once('[') {
            let bracketed = tail.split(']').next().unwrap_or("");
            check_bracketed_host(bracketed)?;
        }
    }
    let fragment = rest.find('#').map_or("", |index| &rest[index + 1..]);
    check_netloc(netloc)?;

    let (userinfo, hostinfo) = match netloc.rfind('@') {
        Some(index) => (Some(&netloc[..index]), &netloc[index + 1..]),
        None => (None, netloc),
    };
    let has_credentials = userinfo.is_some_and(|info| {
        let (user, password) = info.split_once(':').unwrap_or((info, ""));
        !user.is_empty() || !password.is_empty()
    });
    let host = match hostinfo.split_once('[') {
        Some((_, bracketed)) => bracketed.split(']').next().unwrap_or(""),
        None => hostinfo.split(':').next().unwrap_or(""),
    };
    Ok(UrlParts {
        scheme,
        fragment: fragment.to_string(),
        has_credentials,
        hostname: lowercase_host(host),
    })
}

/// `SplitResult.hostname`: lowercased, except an IPv6 zone identifier after
/// `%`, which keeps its case; `None` for an empty host.
fn lowercase_host(host: &str) -> Option<String> {
    if host.is_empty() {
        return None;
    }
    Some(match host.split_once('%') {
        Some((address, zone)) => format!("{}%{zone}", address.to_lowercase()),
        None => host.to_lowercase(),
    })
}

/// Python's `_check_bracketed_host`: the text between `[` and `]` must be an
/// IPv6 literal (optionally with a `%zone`) or an `IPvFuture` address.
fn check_bracketed_host(host: &str) -> Result<(), String> {
    if let Some(future) = host.strip_prefix('v') {
        // re.match(r"\Av[a-fA-F0-9]+\..+\Z", hostname)
        let valid = future.split_once('.').is_some_and(|(version, tail)| {
            !version.is_empty()
                && version.chars().all(|c| c.is_ascii_hexdigit())
                && !tail.is_empty()
                && !tail.contains('\n')
        });
        return if valid {
            Ok(())
        } else {
            Err("IPvFuture address is invalid".to_string())
        };
    }
    if host.parse::<Ipv4Addr>().is_ok() {
        return Err("An IPv4 address cannot be in brackets".to_string());
    }
    let (address, zone) = match host.split_once('%') {
        Some((address, zone)) => (address, Some(zone)),
        None => (host, None),
    };
    let zone_ok = zone.is_none_or(|zone| !zone.is_empty() && !zone.contains('%'));
    if zone_ok && address.parse::<Ipv6Addr>().is_ok() {
        Ok(())
    } else {
        Err(format!(
            "{} does not appear to be an IPv4 or IPv6 address",
            py_repr_str(host)
        ))
    }
}

/// Python's `_checknetloc`: IDNA uses NFKC equivalence, so a non-ASCII netloc
/// whose normalized form gains a URL delimiter is refused.
fn check_netloc(netloc: &str) -> Result<(), String> {
    if netloc.is_empty() || netloc.is_ascii() {
        return Ok(());
    }
    let stripped: String = netloc
        .chars()
        .filter(|c| !matches!(c, '@' | ':' | '#' | '?'))
        .collect();
    let normalized = nfkc(&stripped);
    if stripped == normalized {
        return Ok(());
    }
    if normalized.contains(['/', '?', '#', '@', ':']) {
        return Err(format!(
            "netloc '{netloc}' contains invalid characters under NFKC normalization"
        ));
    }
    Ok(())
}
